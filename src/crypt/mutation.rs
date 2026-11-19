//! Isolated, recoverable commits for encrypted repository mutations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize as _;

use crate::actions::git::{commit, index};

use super::{access, access_storage, format::RepositoryKey, policy, transaction};

const JOURNAL: &str = "mutation.json";
const RECOVERY_KEY: &str = "mutation-key";

pub(crate) fn pending(repo: &Repository) -> Result<bool> {
    Ok(access::private_dir(repo).join(JOURNAL).try_exists()?)
}

pub(crate) fn ownership(repo: &Repository) -> Result<File> {
    let common = repo.commondir().canonicalize()?;
    ensure!(
        std::env::var_os("OVER_COMMIT_REPOSITORY").is_none_or(|path| Path::new(&path) != common),
        "cannot mutate encryption state recursively from a commit hook"
    );
    let directory = access::prepare_private(repo)?;
    let path = directory.join("mutation.lock");
    if path.try_exists()? {
        access::check_private(repo, &path, false)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    #[cfg(windows)]
    access::windows_acl(&path, true)?;
    access::check_private(repo, &path, false)?;
    file.lock()?;
    Ok(file)
}

#[derive(Deserialize, Serialize)]
struct Before {
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
    parent: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Shape {
    source: PathBuf,
    backup: PathBuf,
    file: Option<Vec<u8>>,
    children: access_storage::Files,
    parent: PathBuf,
    identity: Option<String>,
    mode: Option<u32>,
}

impl Drop for Shape {
    fn drop(&mut self) {
        self.file.zeroize();
        for bytes in self.children.values_mut() {
            bytes.zeroize();
        }
    }
}

#[derive(Default)]
struct SensitiveFiles(BTreeMap<PathBuf, Option<Vec<u8>>>);

impl std::ops::Deref for SensitiveFiles {
    type Target = BTreeMap<PathBuf, Option<Vec<u8>>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SensitiveFiles {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for SensitiveFiles {
    fn drop(&mut self) {
        for bytes in self.0.values_mut() {
            bytes.zeroize();
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Journal {
    version: u32,
    common: PathBuf,
    origin: PathBuf,
    head: Vec<u8>,
    parent: Option<String>,
    commit: String,
    before: BTreeMap<PathBuf, Before>,
    after: BTreeMap<PathBuf, Option<Vec<u8>>>,
    shapes: Vec<Shape>,
    directories: Vec<PathBuf>,
    heads: BTreeMap<PathBuf, (Vec<u8>, Option<String>)>,
    old_key: Option<Vec<u8>>,
    new_key: Option<Vec<u8>>,
    #[serde(default)]
    locks: BTreeMap<PathBuf, Vec<u8>>,
}

impl Drop for Journal {
    fn drop(&mut self) {
        for before in self.before.values_mut() {
            before.bytes.zeroize();
        }
        for bytes in self.after.values_mut() {
            bytes.zeroize();
        }
        self.old_key.zeroize();
        self.new_key.zeroize();
    }
}

pub(crate) struct Mutation {
    updates: access_storage::Delta,
    observed: SensitiveFiles,
    metadata: access_storage::Files,
    index: index::IndexSnapshot,
    head: Vec<u8>,
    parent: Option<Oid>,
    tree: Oid,
    next_index: Vec<u8>,
    message: String,
    shapes: Vec<Shape>,
    extra: SensitiveFiles,
    heads: BTreeMap<PathBuf, (Vec<u8>, Option<String>)>,
    inventories: BTreeMap<PathBuf, access_storage::Files>,
    ancestry: Vec<crate::materialize::disposal::PathGuard>,
    preserved: BTreeSet<PathBuf>,
    observed_indexes: BTreeMap<PathBuf, index::IndexSnapshot>,
}

/// Original inputs travel with a computed sibling update. Re-reading them when
/// registering the result would accept and then overwrite intervening staging.
pub(crate) struct LocalObservation {
    directory: PathBuf,
    root: PathBuf,
    head: Vec<u8>,
    parent: Option<Oid>,
    metadata: access_storage::Files,
    observed: SensitiveFiles,
    ancestry: Vec<crate::materialize::disposal::PathGuard>,
    index: Option<git2::Index>,
    index_snapshot: index::IndexSnapshot,
    index_path: PathBuf,
}

impl LocalObservation {
    pub(crate) fn capture(repo: &Repository) -> Result<Self> {
        let index_path = repo.path().join("index");
        // Capture decoded entries and their raw baseline under the same read
        // reservation; otherwise an intervening writer can split those snapshots.
        let _reservation = index::IndexLock::acquire(&index_path)?;
        let mut observation = Self {
            directory: repo.path().canonicalize()?,
            root: access::root(repo)?.to_path_buf(),
            head: fs::read(repo.path().join("HEAD"))?,
            parent: parent(repo)?,
            metadata: access_storage::worktree(repo)?,
            observed: SensitiveFiles::default(),
            ancestry: Vec::new(),
            index: None,
            index_snapshot: index::IndexSnapshot::read(&index_path)?,
            index_path: index_path.clone(),
        };
        observation.observe_file(repo.path().join("index"))?;
        observation.observe_file(observation.root.join(access_storage::ATTRIBUTES))?;
        let mut frozen = git2::Index::new()?;
        if observation
            .observed
            .get(&index_path)
            .is_some_and(Option::is_some)
        {
            let mut source = repo.index()?;
            source.read(true)?;
            for entry in source.iter() {
                frozen.add(&entry)?;
            }
        }
        observation.index = Some(frozen);
        Ok(observation)
    }

    pub(crate) fn parent(&self) -> Option<Oid> {
        self.parent
    }

    pub(crate) fn metadata(&self) -> &access_storage::Files {
        &self.metadata
    }

    pub(crate) fn take_index(&mut self) -> Result<git2::Index> {
        self.index
            .take()
            .context("rotation index snapshot has already been consumed")
    }

    pub(crate) fn observe_file(&mut self, path: PathBuf) -> Result<()> {
        if !self.observed.contains_key(&path) {
            self.ancestry.push(parent_guard(&path)?);
            self.observed.insert(path.clone(), optional(&path)?);
        }
        Ok(())
    }

    pub(crate) fn observe_metadata(&mut self, path: &Path) -> Result<()> {
        ensure!(
            access_storage::is_path(path),
            "expected an access metadata path"
        );
        let absolute = self.root.join(path);
        self.ancestry.push(parent_guard(&absolute)?);
        // Absence is already known from the complete captured namespace; never
        // re-baseline a newly appeared grant after computing the metadata delta.
        self.observed
            .insert(absolute, self.metadata.get(path).cloned());
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        let repo = Repository::open(&self.directory)?;
        ensure!(
            fs::read(self.directory.join("HEAD"))? == self.head && parent(&repo)? == self.parent,
            "participating worktree HEAD changed during rotation preparation"
        );
        ensure!(
            access_storage::worktree(&repo)? == self.metadata,
            "participating access inventory changed during rotation preparation"
        );
        for ancestry in &self.ancestry {
            ancestry.verify()?;
        }
        for (path, expected) in self.observed.iter() {
            let current = zeroize::Zeroizing::new(optional(path)?);
            if path == &self.index_path {
                ensure!(
                    index::IndexSnapshot::from_bytes(current.as_deref())? == self.index_snapshot,
                    "participating worktree index changed during rotation preparation"
                );
                continue;
            }
            ensure!(
                *current == *expected,
                "participating worktree input changed during rotation preparation"
            );
        }
        Ok(())
    }
}

fn parent(repo: &Repository) -> Result<Option<Oid>> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit()?.id())),
        Err(error) if error.code() == git2::ErrorCode::UnbornBranch => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.is_symlink(),
                "mutation paths must be real regular files"
            );
            Ok(Some(fs::read(path)?))
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn children(path: &Path) -> Result<access_storage::Files> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.is_symlink(),
        "migration directory must be a real directory"
    );
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let bytes = optional(&entry.path())?.context("migration entry disappeared")?;
        files.insert(PathBuf::from(entry.file_name()), bytes);
    }
    Ok(files)
}

impl Mutation {
    pub(crate) fn prepare(
        repo: &Repository,
        updates: access_storage::Delta,
        message: String,
        transitions: &[PathBuf],
    ) -> Result<Self> {
        ensure!(
            repo.state() == git2::RepositoryState::Clean,
            "finish the current Git operation before changing encrypted state"
        );
        let root = access::root(repo)?;
        let head = fs::read(repo.path().join("HEAD"))?;
        let parent = parent(repo)?;
        let base = match parent {
            Some(oid) => repo.find_commit(oid)?.tree()?,
            None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
        };
        // Keep the replacement input and validation baseline consistent, without
        // holding Git's lock across hooks or interactive signing.
        let (original_index, mut staged) = {
            let path = repo.path().join("index");
            let _reservation = index::IndexLock::acquire(&path)?;
            let original = index::IndexSnapshot::read(&path)?;
            let mut staged = repo.index()?;
            staged.read(true)?;
            (original, staged)
        };
        ensure!(
            !staged.has_conflicts(),
            "resolve index conflicts before changing encrypted state"
        );
        let mut shapes = Vec::new();
        for relative in transitions {
            let source = root.join(relative);
            crate::materialize::disposal::validate_relative_ancestors(root, &source)?;
            let metadata = fs::symlink_metadata(&source)?;
            ensure!(
                !metadata.is_symlink(),
                "migration source must not be a symlink"
            );
            let temporary = tempfile::Builder::new()
                .prefix("legacy-state-backup-")
                .tempdir_in(access::private_dir(repo))?;
            let backup = temporary.path().to_path_buf();
            drop(temporary);
            shapes.push(Shape {
                source: source.clone(),
                backup,
                file: if metadata.is_file() {
                    optional(&source)?
                } else {
                    None
                },
                children: if metadata.is_dir() {
                    children(&source)?
                } else {
                    BTreeMap::new()
                },
                parent: crate::overlays::mapping::location(
                    source.parent().context("migration source has no parent")?,
                )?,
                #[cfg(unix)]
                identity: Some(transaction::file_identity(&source)?),
                #[cfg(not(unix))]
                identity: None,
                #[cfg(unix)]
                mode: {
                    use std::os::unix::fs::PermissionsExt as _;
                    Some(metadata.permissions().mode() & 0o7777)
                },
                #[cfg(not(unix))]
                mode: None,
            });
        }
        let mut observed = SensitiveFiles::default();
        let attributes = root.join(access_storage::ATTRIBUTES);
        observed.insert(attributes.clone(), optional(&attributes)?);
        let mut candidate = git2::Index::new()?;
        candidate.read_tree(&base)?;
        let mut ordered: Vec<_> = updates.iter().collect();
        ordered.sort_by_key(|(_, after)| after.is_some());
        for (relative, after) in ordered {
            ensure!(
                relative.is_relative()
                    && !relative.components().any(|component| matches!(
                        component,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )),
                "mutation path must be repository relative"
            );
            let path = root.join(relative);
            let shape_root = shapes.iter().any(|shape| shape.source == path);
            if !shape_root {
                observed.insert(path.clone(), optional(&path)?);
            }
            let old = base.get_path(relative).ok();
            let entry = staged.get_path(relative, 0);
            // Owned tracked changes must start clean; the caller cannot accidentally
            // fold earlier staged content into an access or secret mutation.
            if let Some(old) = old
                .as_ref()
                .filter(|entry| entry.kind() != Some(git2::ObjectType::Tree))
            {
                ensure!(
                    entry.as_ref().is_some_and(
                        |entry| entry.id == old.id() && entry.mode == old.filemode() as u32
                    ),
                    "mutation path '{}' has staged changes; commit or restore them first",
                    relative.display()
                );
                if shape_root {
                    let source = shapes
                        .iter()
                        .find(|shape| shape.source == path)
                        .context("migration source disappeared")?;
                    ensure!(
                        source.file.as_ref().is_some_and(|bytes| Oid::hash_object(
                            git2::ObjectType::Blob,
                            bytes
                        )
                        .ok()
                            == Some(old.id())),
                        "migration source has pending changes; commit or restore them first"
                    );
                } else {
                    let bytes = observed.get(&path).and_then(Option::as_ref);
                    // Rotation may supply a clean unlocked representation of a protected blob.
                    let stored = repo.find_blob(old.id())?;
                    let matches = bytes.is_some_and(|bytes| {
                        policy::bootstrap_equal(relative, stored.content(), bytes)
                    }) || (super::format::recognized(stored.content())
                        && bytes.is_some_and(|bytes| {
                            access::local_key(repo)
                                .and_then(|key| {
                                    super::context::decrypt(&key, stored.content(), relative)
                                })
                                .is_ok_and(|plain| plain.as_slice() == bytes.as_slice())
                        }));
                    ensure!(
                        matches,
                        "mutation path '{}' has unstaged changes; commit or restore them first",
                        relative.display()
                    );
                }
            } else if let Some(entry) = &entry {
                ensure!(
                    observed
                        .get(&path)
                        .and_then(Option::as_ref)
                        .is_some_and(|bytes| Oid::hash_object(git2::ObjectType::Blob, bytes).ok()
                            == Some(entry.id)),
                    "mutation path has different staged changes"
                );
            }
            if let Some(bytes) = after {
                // Attribute blobs use Git's canonical LF representation while
                // their prepared worktree output retains the checkout's EOLs.
                let canonical = relative
                    .file_name()
                    .is_some_and(|name| name == ".gitattributes")
                    .then(|| policy::attribute_bytes(bytes));
                policy::stage_blob(
                    &mut staged,
                    repo,
                    relative,
                    canonical.as_deref().unwrap_or(bytes),
                )?;
                candidate.add(
                    &staged
                        .get_path(relative, 0)
                        .context("prepared entry disappeared")?,
                )?;
            } else {
                if staged.get_path(relative, 0).is_some() {
                    staged.remove_path(relative)?;
                }
                if candidate.get_path(relative, 0).is_some() {
                    candidate.remove_path(relative)?;
                }
            }
        }
        let tree = candidate.write_tree_to(repo)?;
        policy::validate_tree(repo, &repo.find_tree(tree)?)?;
        let mut ancestry = Vec::new();
        for path in observed
            .keys()
            .chain(shapes.iter().map(|shape| &shape.source))
        {
            let input = shapes
                .iter()
                .find(|shape| path.starts_with(&shape.source))
                .map(|shape| &shape.source)
                .unwrap_or(path);
            ancestry.push(parent_guard(input)?);
        }
        Ok(Self {
            updates,
            observed,
            metadata: access_storage::worktree(repo)?,
            index: original_index,
            head,
            parent,
            tree,
            next_index: policy::index_bytes(repo, &staged)?,
            message,
            shapes,
            extra: SensitiveFiles::default(),
            heads: BTreeMap::new(),
            inventories: BTreeMap::new(),
            ancestry,
            preserved: BTreeSet::new(),
            observed_indexes: BTreeMap::new(),
        })
    }

    /// Local updates (including sibling indexes) never enter the initiating commit tree.
    pub(crate) fn require_protected(&self, repo: &Repository, paths: &[PathBuf]) -> Result<()> {
        let directory = tempfile::tempdir_in(repo.path())?;
        let path = directory.path().join("index");
        let mut index = git2::Index::open(&path)?;
        index.read_tree(&repo.find_tree(self.tree)?)?;
        index.write()?;
        for entry in paths {
            for (attribute, expected) in [
                ("filter", b"over".as_slice()),
                ("diff", b"over-secret".as_slice()),
            ] {
                let output = policy::git_command(repo)?
                    .args(["check-attr", "--cached", "-z", attribute, "--"])
                    .arg(entry)
                    .env("GIT_INDEX_FILE", &path)
                    .output()?;
                ensure!(
                    output.status.success()
                        && output.stdout.split(|byte| *byte == 0).nth(2) == Some(expected),
                    "native secret entries must retain filter=over and diff=over-secret"
                );
            }
        }
        Ok(())
    }

    /// Local updates (including sibling indexes) never enter the initiating commit tree.
    pub(crate) fn add_local(
        &mut self,
        mut observation: LocalObservation,
        updates: BTreeMap<PathBuf, Option<Vec<u8>>>,
    ) -> Result<()> {
        observation.validate()?;
        self.inventories
            .insert(observation.directory.clone(), observation.metadata);
        self.heads.insert(
            observation.directory,
            (
                observation.head,
                observation.parent.map(|oid| oid.to_string()),
            ),
        );
        self.ancestry.extend(observation.ancestry);
        for (path, before) in observation.observed.iter() {
            if let Some(existing) = self.observed.get(path) {
                if let Some(snapshot) = self.observed_indexes.get(path) {
                    ensure!(
                        index::IndexSnapshot::from_bytes(before.as_deref())? == *snapshot,
                        "mutation index changed before local-update registration"
                    );
                    continue;
                }
                ensure!(
                    existing == before,
                    "mutation input changed before local-update registration"
                );
            }
        }
        self.observed_indexes
            .insert(observation.index_path, observation.index_snapshot);
        for (path, before) in &mut observation.observed.0 {
            if !self.observed.contains_key(path) {
                self.observed.insert(path.clone(), std::mem::take(before));
            }
        }
        for (path, bytes) in updates {
            ensure!(
                self.observed.contains_key(&path),
                "local update has no original observation"
            );
            self.extra.insert(path, bytes);
        }
        Ok(())
    }

    /// Keep an already-correct unlocked representation without copying plaintext
    /// into another publication buffer or writing it back to the filesystem.
    pub(crate) fn preserve_worktree(&mut self, path: PathBuf) {
        self.preserved.insert(path);
    }

    fn validate(&self, repo: &Repository) -> Result<()> {
        for ancestry in &self.ancestry {
            ancestry.verify()?;
        }
        ensure!(
            fs::read(repo.path().join("HEAD"))? == self.head && parent(repo)? == self.parent,
            "HEAD changed while preparing mutation; retry"
        );
        ensure!(
            index::IndexSnapshot::read(&repo.path().join("index"))? == self.index,
            "index changed while preparing mutation; retry"
        );
        ensure!(
            access_storage::worktree(repo)? == self.metadata,
            "access metadata changed while preparing mutation; retry"
        );
        for (path, bytes) in self.observed.iter() {
            let current = zeroize::Zeroizing::new(optional(path)?);
            if let Some(snapshot) = self.observed_indexes.get(path) {
                ensure!(
                    index::IndexSnapshot::from_bytes(current.as_deref())? == *snapshot,
                    "mutation index changed; preserve concurrent staging and retry"
                );
                continue;
            }
            ensure!(
                *current == *bytes,
                "mutation input changed; preserve hook/concurrent edits and retry"
            );
        }
        for shape in &self.shapes {
            ensure!(
                !shape.backup.try_exists()?,
                "migration backup path became occupied"
            );
            if let Some(bytes) = &shape.file {
                ensure!(
                    optional(&shape.source)?.as_ref() == Some(bytes),
                    "migration source changed"
                );
            } else {
                ensure!(
                    children(&shape.source)? == shape.children,
                    "migration directory changed"
                );
            }
        }
        for (path, (head, old)) in &self.heads {
            ensure!(
                fs::read(path.join("HEAD"))? == *head
                    && parent(&Repository::open(path)?)?.map(|oid| oid.to_string()) == *old,
                "participating worktree HEAD changed"
            );
        }
        for (path, files) in &self.inventories {
            ensure!(
                access_storage::worktree(&Repository::open(path)?)? == *files,
                "participating access inventory changed while preparing mutation"
            );
        }
        Ok(())
    }

    pub(crate) fn commit(
        mut self,
        repo: &Repository,
        key: &RepositoryKey,
        next_key: Option<&RepositoryKey>,
    ) -> Result<()> {
        self.validate(repo)?;
        let prepared =
            commit::PreparedCommit::prepare(repo, self.tree, &self.message, self.parent)?;
        let guard = access::guard(repo, true)?;
        let index_path = repo.path().join("index");
        let mut locks = BTreeMap::new();
        for path in self
            .extra
            .keys()
            .filter(|path| {
                path.file_name().is_some_and(|name| name == "index")
                    && path.starts_with(repo.commondir())
            })
            .chain(std::iter::once(&index_path))
        {
            locks.insert(path.clone(), index::IndexLock::acquire(path)?);
        }
        self.validate(repo)?;
        prepared.validate(repo)?;
        let root = access::root(repo)?;
        let mut after: BTreeMap<_, _> = self
            .updates
            .iter()
            .filter(|(path, _)| !self.preserved.contains(&root.join(path)))
            .map(|(path, bytes)| (root.join(path), bytes.clone()))
            .collect();
        // The index snapshot and unlocked worktree representation can differ.
        after.extend(std::mem::take(&mut self.extra.0));
        after.insert(index_path, Some(self.next_index));
        let mut journal = Journal {
            version: 1,
            common: repo.commondir().canonicalize()?,
            origin: repo.path().canonicalize()?,
            head: self.head,
            parent: self.parent.map(|oid| oid.to_string()),
            commit: prepared.oid.to_string(),
            before: BTreeMap::new(),
            after,
            shapes: self.shapes,
            directories: Vec::new(),
            heads: self.heads,
            old_key: optional(&access::private_dir(repo).join("key"))?,
            new_key: next_key.map(|key| key.encode().to_vec()),
            locks: BTreeMap::new(),
        };
        for (path, lock) in &mut locks {
            journal
                .locks
                .insert(lock_path(path), lock.mark_ownership()?);
        }
        for path in journal.after.keys() {
            if journal
                .shapes
                .iter()
                .any(|shape| path.starts_with(&shape.source))
            {
                continue;
            }
            let mut parent = path.parent().context("mutation path has no parent")?;
            while !parent.try_exists()? {
                if !journal.directories.contains(&parent.to_path_buf()) {
                    journal.directories.push(parent.to_path_buf());
                }
                parent = parent.parent().context("mutation parent has no ancestor")?;
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt as _;
                fs::metadata(path)
                    .ok()
                    .map(|metadata| metadata.permissions().mode() & 0o7777)
            };
            #[cfg(not(unix))]
            let mode = None;
            journal.before.insert(
                path.clone(),
                Before {
                    bytes: optional(path)?,
                    mode,
                    parent: crate::overlays::mapping::location(
                        path.parent().context("mutation path has no parent")?,
                    )?,
                },
            );
        }
        // A pending key permits recovery even before first enrollment has an active key.
        super::atomic_write(&access::private_dir(repo).join(RECOVERY_KEY), &key.encode())?;
        super::atomic_write(
            &access::private_dir(repo).join(JOURNAL),
            &key.encrypt(&super::buffer::to_json(&journal)?, 1)?,
        )?;
        let result = (|| -> Result<()> {
            for shape in &journal.shapes {
                fs::rename(&shape.source, &shape.backup)?;
                #[cfg(unix)]
                {
                    File::open(
                        shape
                            .source
                            .parent()
                            .context("migration source has no parent")?,
                    )?
                    .sync_all()?;
                    File::open(access::private_dir(repo))?.sync_all()?;
                }
            }
            for (path, bytes) in &journal.after {
                if journal.shapes.iter().any(|shape| shape.source == *path) && bytes.is_none() {
                    continue;
                }
                if let Some(bytes) = bytes {
                    if let Some(lock) = locks.get_mut(path) {
                        lock.replace_while_locked(path, bytes)?;
                    } else if optional(path)?.as_ref() != Some(bytes) {
                        transaction::worktree_write(path, bytes, None)?;
                    }
                } else if path.try_exists()? {
                    fs::remove_file(path)?;
                }
            }
            drop(guard);
            prepared.publish(repo)?;
            Ok(())
        })();
        drop(locks);
        if let Err(error) = result {
            let published = match parent(repo)? {
                Some(oid) => oid == prepared.oid || repo.graph_descendant_of(oid, prepared.oid)?,
                None => false,
            };
            policy::recover_source(repo)
                .with_context(|| format!("{error:#}; mutation recovery also failed"))?;
            if published {
                tracing::warn!(
                    "commit {} succeeded despite Git's publication error: {error:#}",
                    prepared.oid
                );
                commit::post_commit(repo);
                return Ok(());
            }
            return Err(error);
        }
        if let Err(error) = policy::recover_source(repo) {
            tracing::warn!(
                "commit {} succeeded; run over crypt recover to finalize it: {error:#}",
                prepared.oid
            );
            return Ok(());
        }
        commit::post_commit(repo);
        Ok(())
    }
}

pub(crate) fn clean_metadata(repo: &Repository) -> Result<access_storage::Files> {
    let files = access_storage::worktree(repo)?;
    let index = repo.index()?;
    let indexed = access_storage::index(repo, &index)?;
    let head = parent(repo)?
        .map(|oid| repo.find_commit(oid)?.tree())
        .transpose()?;
    let committed = head
        .as_ref()
        .map(|tree| access_storage::tree(repo, tree))
        .transpose()?
        .unwrap_or_default();
    ensure!(
        (committed.is_empty() || committed == files)
            && indexed
                .iter()
                .all(|(path, bytes)| files.get(path) == Some(bytes))
            && committed.keys().all(|path| indexed.contains_key(path)),
        "commit or restore pending access metadata changes first"
    );
    for path in committed.keys() {
        let staged = index
            .get_path(path, 0)
            .context("access metadata disappeared from index")?;
        ensure!(
            head.as_ref()
                .context("access metadata has no HEAD")?
                .get_path(path)?
                .filemode() as u32
                == staged.mode,
            "commit or restore staged access metadata mode changes first"
        );
        if crate::materialize::virtual_checkout::snapshot::honors_filemode(repo)? {
            let actual = crate::materialize::virtual_checkout::git::on_disk_filemode(
                &access::root(repo)?.join(path),
            )?;
            ensure!(
                i32::from(actual) as u32 == staged.mode,
                "commit or restore access metadata executable-bit changes first"
            );
        }
    }
    Ok(files)
}

/// Caller holds raw lifecycle ownership; normal key reads deliberately refuse journals.
pub(crate) fn recover_locked(repo: &Repository) -> Result<()> {
    let directory = access::private_dir(repo);
    let key_path = directory.join(RECOVERY_KEY);
    access::check_private(repo, &key_path, false)?;
    ensure!(
        fs::metadata(&key_path)?.len() == 96,
        "invalid mutation recovery key"
    );
    let bytes = super::buffer::read_file(&key_path)?;
    let key = RepositoryKey::decode(&bytes)?;
    let journal_path = directory.join(JOURNAL);
    access::check_private(repo, &journal_path, false)?;
    ensure!(
        fs::metadata(&journal_path)?.len()
            <= (super::format::MAX_BYTES + super::format::HEADER_LEN + 16) as u64,
        "mutation journal exceeds the supported size limit"
    );
    let plaintext = key.decrypt(&fs::read(&journal_path)?, 1)?;
    let mut journal: Journal =
        serde_json::from_slice(&plaintext).context("invalid mutation journal")?;
    ensure!(
        journal.version == 1 && journal.common == repo.commondir().canonicalize()?,
        "unsupported or foreign mutation journal"
    );
    let origin = Repository::open(&journal.origin)?;
    let expected = Oid::from_str(&journal.commit)?;
    let published = match parent(&origin)? {
        Some(current) => current == expected || origin.graph_descendant_of(current, expected)?,
        None => false,
    };
    for (path, token) in &journal.locks {
        ensure!(
            path.starts_with(repo.commondir())
                && path.file_name().is_some_and(|name| name == "index.lock"),
            "invalid journaled index lock"
        );
        if let Some(current) = optional(path)? {
            ensure!(
                current == *token,
                "index is locked by another writer; inspect that process before recovery"
            );
            fs::remove_file(path)?;
            #[cfg(unix)]
            File::open(path.parent().context("index lock has no parent")?)?.sync_all()?;
        }
    }
    for (path, (head, old)) in &journal.heads {
        if *path == journal.origin && published {
            continue;
        }
        ensure!(
            fs::read(path.join("HEAD"))? == *head
                && parent(&Repository::open(path)?)?.map(|oid| oid.to_string()) == *old,
            "participating worktree changed before recovery"
        );
    }
    if published {
        if let Some(bytes) = &journal.new_key {
            let next = RepositoryKey::decode(bytes)?;
            let current = zeroize::Zeroizing::new(optional(&directory.join("key"))?);
            ensure!(
                *current == journal.old_key || current.as_ref() == Some(bytes),
                "active key changed independently before finalization"
            );
            let registry = access::load(&origin)?;
            ensure!(
                registry.repository == next.repository && registry.generation == next.generation,
                "published access metadata no longer matches the pending key"
            );
            if current.as_ref() != Some(bytes) {
                access::install(repo, &next)?;
            }
        }
    } else {
        ensure!(
            fs::read(journal.origin.join("HEAD"))? == journal.head
                && parent(&origin)?.map(|oid| oid.to_string()) == journal.parent,
            "HEAD changed before recovery; preserve that branch first"
        );
        let mut locks = BTreeMap::new();
        for path in journal.before.keys().filter(|path| {
            path.starts_with(repo.commondir())
                && path.file_name().is_some_and(|name| name == "index")
        }) {
            locks.insert(path.clone(), index::IndexLock::acquire(path)?);
        }
        journal.locks.clear();
        for (path, lock) in &mut locks {
            journal
                .locks
                .insert(lock_path(path), lock.mark_ownership()?);
        }
        // A second interrupted recovery must recognize its new reservation too.
        super::atomic_write(
            &directory.join(JOURNAL),
            &key.encrypt(&super::buffer::to_json(&journal)?, 1)?,
        )?;
        for (path, old) in &journal.before {
            #[cfg(unix)]
            if let Ok(metadata) = fs::symlink_metadata(path) {
                use std::os::unix::fs::PermissionsExt as _;
                ensure!(
                    metadata.permissions().mode() & 0o7777 == old.mode.unwrap_or(0o600),
                    "mutation permissions changed after interruption; preserve that edit before recovery"
                );
            }
            ensure!(
                crate::overlays::mapping::location(
                    path.parent().context("recovery path has no parent")?
                )? == old.parent,
                "mutation ancestry changed before recovery"
            );
            let current = zeroize::Zeroizing::new(optional(path)?);
            ensure!(
                *current == old.bytes
                    || journal
                        .after
                        .get(path)
                        .is_some_and(|expected| *expected == *current),
                "mutation path changed after interruption; preserve edits before recovery"
            );
        }
        for shape in &journal.shapes {
            ensure!(
                shape.backup.parent() == Some(directory.as_path()),
                "invalid shape backup path"
            );
            #[cfg(unix)]
            if let Some(mode) = shape.mode {
                use std::os::unix::fs::PermissionsExt as _;
                let original = if shape.backup.try_exists()? {
                    &shape.backup
                } else {
                    &shape.source
                };
                ensure!(
                    fs::symlink_metadata(original)?.permissions().mode() & 0o7777 == mode,
                    "migration source permissions changed before recovery"
                );
            }
            ensure!(
                crate::overlays::mapping::location(
                    shape
                        .source
                        .parent()
                        .context("migration source has no parent")?
                )? == shape.parent,
                "migration ancestry changed before recovery"
            );
            if shape.backup.try_exists()? {
                if let Some(identity) = &shape.identity {
                    ensure!(
                        transaction::file_identity(&shape.backup)? == *identity,
                        "migration backup identity changed before recovery"
                    );
                }
                if let Some(bytes) = &shape.file {
                    ensure!(
                        optional(&shape.backup)?.as_ref() == Some(bytes),
                        "migration backup changed"
                    );
                } else {
                    ensure!(
                        children(&shape.backup)? == shape.children,
                        "migration backup changed"
                    );
                }
                if shape.source.try_exists()? {
                    let actual = children(&shape.source)?;
                    let expected: access_storage::Files = journal
                        .after
                        .iter()
                        .filter_map(|(path, bytes)| {
                            path.strip_prefix(&shape.source)
                                .ok()
                                .filter(|path| path.components().count() == 1)
                                .and_then(|path| {
                                    bytes
                                        .as_ref()
                                        .map(|bytes| (path.to_path_buf(), bytes.clone()))
                                })
                        })
                        .collect();
                    ensure!(
                        actual
                            .iter()
                            .all(|(path, bytes)| expected.get(path) == Some(bytes)),
                        "migration directory has intervening edits; preserve them before recovery"
                    );
                }
            } else if let Some(bytes) = &shape.file {
                if let Some(identity) = &shape.identity {
                    ensure!(
                        transaction::file_identity(&shape.source)? == *identity,
                        "migration source identity changed before recovery"
                    );
                }
                ensure!(
                    optional(&shape.source)?.as_ref() == Some(bytes),
                    "migration source disappeared"
                );
            } else {
                ensure!(
                    children(&shape.source)? == shape.children,
                    "migration source changed"
                );
            }
        }
        for shape in &journal.shapes {
            if !shape.backup.try_exists()? {
                continue;
            }
            if shape.source.try_exists()? {
                for child in children(&shape.source)?.keys() {
                    fs::remove_file(shape.source.join(child))?;
                }
                fs::remove_dir(&shape.source)?;
            }
            fs::rename(&shape.backup, &shape.source)?;
        }
        // Restore the index last, keeping cooperating writers out during rollback.
        let mut paths: Vec<_> = journal.before.keys().cloned().collect();
        paths.sort_by_key(|path| locks.contains_key(path));
        for path in paths {
            let old = &journal.before[&path];
            if let Some(bytes) = &old.bytes {
                let current = zeroize::Zeroizing::new(optional(&path)?);
                if current.as_ref() != Some(bytes) {
                    if let Some(lock) = locks.get_mut(&path) {
                        lock.replace_while_locked(&path, bytes)?;
                    } else {
                        transaction::worktree_write(&path, bytes, None)?;
                    }
                }
                #[cfg(unix)]
                if let Some(mode) = old.mode {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
                }
            } else if path.try_exists()? {
                fs::remove_file(path)?;
            }
        }
        journal
            .directories
            .sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for path in &journal.directories {
            match fs::remove_dir(path) {
                Ok(()) => (),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    fs::remove_file(directory.join(JOURNAL))?;
    #[cfg(unix)]
    File::open(&directory)?.sync_all()?;
    fs::remove_file(key_path)?;
    #[cfg(unix)]
    File::open(&directory)?.sync_all()?;
    Ok(())
}

pub(crate) fn cleanup_orphan(repo: &Repository) -> Result<()> {
    let path = access::private_dir(repo).join(RECOVERY_KEY);
    if !pending(repo)? && path.try_exists()? {
        access::check_private(repo, &path, false)?;
        ensure!(
            fs::metadata(&path)?.len() == 96,
            "unexpected pending mutation key"
        );
        fs::remove_file(path)?;
    }
    Ok(())
}

fn lock_path(index: &Path) -> PathBuf {
    let mut path = index.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn parent_guard(path: &Path) -> Result<crate::materialize::disposal::PathGuard> {
    let mut parent = path.parent().context("mutation path has no parent")?;
    while !parent.try_exists()? {
        parent = parent
            .parent()
            .context("mutation path has no existing ancestor")?;
    }
    crate::materialize::disposal::PathGuard::capture(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_index_refreshes_are_accepted_before_and_after_registration() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        fs::write(directory.path().join("unrelated"), b"staged work").unwrap();
        let mut staged = repo.index().unwrap();
        staged.add_path(Path::new("unrelated")).unwrap();
        staged.write().unwrap();
        let observation = LocalObservation::capture(&repo).unwrap();
        let mut entry = staged.get(0).unwrap();
        entry.mtime = git2::IndexTime::new(1, 0);
        staged.add(&entry).unwrap();
        staged.write().unwrap();
        observation.validate().unwrap();
        let mut mutation = Mutation::prepare(
            &repo,
            [(PathBuf::from("managed"), Some(b"planned".to_vec()))].into(),
            "chore: planned".into(),
            &[],
        )
        .unwrap();
        mutation.add_local(observation, BTreeMap::new()).unwrap();
        // Model an independent Git writer, not libgit2's shared in-memory index
        // that Mutation::prepare has already changed for its future publication.
        let mut staged = git2::Index::open(&repo.path().join("index")).unwrap();
        staged.set_version(4).unwrap();
        staged.write().unwrap();
        mutation.validate(&repo).unwrap();
        // Registration must compare against the original logical baseline too.
        mutation
            .add_local(LocalObservation::capture(&repo).unwrap(), BTreeMap::new())
            .unwrap();
    }

    #[test]
    fn stale_sibling_index_replacements_cannot_adopt_new_unrelated_staging() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        let observation = LocalObservation::capture(&repo).unwrap();
        let replacement = policy::index_bytes(&repo, &git2::Index::new().unwrap()).unwrap();
        fs::write(directory.path().join("unrelated"), b"concurrent staging").unwrap();
        let mut index = repo.index().unwrap();
        policy::stage_blob(
            &mut index,
            &repo,
            Path::new("unrelated"),
            b"concurrent staging",
        )
        .unwrap();
        index.write().unwrap();
        let concurrent = fs::read(repo.path().join("index")).unwrap();
        let mut mutation = Mutation::prepare(
            &repo,
            [(PathBuf::from("managed"), Some(b"planned".to_vec()))].into(),
            "chore: planned".into(),
            &[],
        )
        .unwrap();
        assert!(
            mutation
                .add_local(
                    observation,
                    [(repo.path().join("index"), Some(replacement))].into()
                )
                .is_err()
        );
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), concurrent);
        assert!(
            repo.index()
                .unwrap()
                .get_path(Path::new("unrelated"), 0)
                .is_some()
        );
    }

    #[test]
    fn local_update_registration_rejects_edits_after_the_original_file_read() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        let path = directory.path().join("local");
        fs::write(&path, b"original").unwrap();
        let mut observation = LocalObservation::capture(&repo).unwrap();
        observation.observe_file(path.clone()).unwrap();
        fs::write(&path, b"independent edit").unwrap();
        let mut mutation = Mutation::prepare(
            &repo,
            [(PathBuf::from("managed"), Some(b"planned".to_vec()))].into(),
            "chore: planned".into(),
            &[],
        )
        .unwrap();
        assert!(
            mutation
                .add_local(
                    observation,
                    [(path.clone(), Some(b"stale result".to_vec()))].into()
                )
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"independent edit");
    }
}
