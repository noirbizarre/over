//! Publish only the native store and its required bootstrap metadata.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};

use crate::actions::git::{commit, index};
use crate::crypt::{access, policy};

pub(super) fn mutation_guard(repo: &Repository) -> Result<fs::File> {
    ensure!(
        std::env::var_os("OVER_COMMIT_REPOSITORY")
            .is_none_or(|path| path != repo.path().as_os_str()),
        "cannot mutate secrets recursively from a commit hook"
    );
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(repo.commondir().join("over-secrets.lock"))?;
    // This separate advisory lock serializes writers while hooks can still read
    // secrets or invoke clean filters through the unlocked lifecycle guard.
    file.lock()?;
    Ok(file)
}

pub(super) struct Mutation {
    updates: BTreeMap<PathBuf, Option<Vec<u8>>>,
    originals: BTreeMap<PathBuf, Option<Vec<u8>>>,
    index: Option<Vec<u8>>,
    head: Vec<u8>,
    parent: Option<Oid>,
    tree: Oid,
    next_index: Vec<u8>,
    message: String,
    directory: Option<(PathBuf, BTreeMap<PathBuf, Vec<u8>>)>,
}

impl Mutation {
    pub(super) fn prepare(
        repo: &Repository,
        mut updates: BTreeMap<PathBuf, Option<Vec<u8>>>,
        message: String,
    ) -> Result<Self> {
        ensure!(
            repo.state() == git2::RepositoryState::Clean,
            "finish the current Git operation before changing secrets"
        );
        let root = access::root(repo)?;
        let head = fs::read(repo.path().join("HEAD"))?;
        let parent = repo
            .head()
            .ok()
            .map(|head| head.peel_to_commit())
            .transpose()?;
        let base = match &parent {
            Some(parent) => parent.tree()?,
            None => repo.find_tree(repo.treebuilder(None)?.write()?)?,
        };
        let original_index = index::read_index(&repo.path().join("index"))?;
        let mut staged = repo.index()?;
        let directory_path = root.join(super::layout::PATH);
        let directory = if fs::symlink_metadata(&directory_path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.is_symlink())
        {
            let temporary = tempfile::Builder::new()
                .prefix("legacy-secrets-backup-")
                .tempdir_in(access::private_dir(repo))?;
            let backup = temporary.path().to_path_buf();
            drop(temporary);
            Some((backup, super::layout::legacy_files(&directory_path)?))
        } else {
            None
        };
        ensure!(
            !staged.has_conflicts(),
            "resolve index conflicts before changing secrets"
        );
        let mut metadata_snapshots = BTreeMap::new();
        let metadata = crate::crypt::access_storage::worktree(repo)?;
        let indexed_metadata = crate::crypt::access_storage::index(repo, &staged)?;
        ensure!(
            indexed_metadata
                .iter()
                .all(|(path, bytes)| metadata.get(path) == Some(bytes)),
            "crypt metadata has different staged changes"
        );
        let committed_metadata = crate::crypt::access_storage::tree(repo, &base)?;
        ensure!(
            committed_metadata.is_empty() || committed_metadata == metadata,
            "commit pending crypt metadata changes before changing secrets"
        );
        let mut bootstrap: Vec<_> = metadata.keys().cloned().collect();
        bootstrap.push(PathBuf::from(crate::crypt::access_storage::ATTRIBUTES));
        for path in bootstrap {
            let bytes = fs::read(root.join(&path)).context("missing secret bootstrap metadata")?;
            let id = Oid::hash_object(git2::ObjectType::Blob, &bytes)?;
            metadata_snapshots.insert(path.clone(), Some(bytes.clone()));
            if let Ok(old) = base.get_path(&path) {
                ensure!(
                    old.id() == id
                        && staged.get_path(&path, 0).is_some_and(
                            |entry| entry.id == id && entry.mode == old.filemode() as u32
                        ),
                    "commit pending crypt metadata changes before changing secrets"
                );
            } else {
                ensure!(
                    staged.get_path(&path, 0).is_none_or(|entry| entry.id == id),
                    "crypt bootstrap metadata has different staged changes"
                );
                updates.entry(path).or_insert(Some(bytes));
            }
        }
        let mut originals = metadata_snapshots;
        let mut candidate = git2::Index::new()?;
        candidate.read_tree(&base)?;
        let mut ordered: Vec<_> = updates.iter().collect();
        // Remove historical directory children before installing the aggregate index entry.
        ordered.sort_by_key(|(_, next)| next.is_some());
        for (path, next) in ordered {
            let absolute = root.join(path);
            let replacing_directory = directory.is_some() && path == Path::new(super::layout::PATH);
            if let Ok(metadata) = fs::symlink_metadata(&absolute) {
                ensure!(
                    (metadata.is_file() || replacing_directory) && !metadata.is_symlink(),
                    "secret commit paths must be regular files"
                );
            }
            let current = if replacing_directory {
                None
            } else {
                index::read_index(&absolute)?
            };
            let old = base.get_path(path).ok();
            let staged_entry = staged.get_path(path, 0);
            if replacing_directory {
                ensure!(
                    old.is_none_or(|entry| entry.kind() == Some(git2::ObjectType::Tree)),
                    "legacy store conflicts with the committed aggregate"
                );
            } else if let Some(old) = old {
                ensure!(
                    staged_entry
                        .as_ref()
                        .is_some_and(|entry| entry.id == old.id())
                        && current.as_ref().is_some_and(|bytes| Oid::hash_object(
                            git2::ObjectType::Blob,
                            bytes
                        )
                        .ok()
                            == Some(old.id())),
                    "secret commit path '{}' has pending changes; commit or restore them first",
                    path.display()
                );
            } else {
                ensure!(
                    staged_entry.as_ref().is_none_or(|entry| current
                        .as_ref()
                        .is_some_and(|bytes| Oid::hash_object(git2::ObjectType::Blob, bytes).ok()
                            == Some(entry.id))),
                    "secret commit path has different staged changes"
                );
            }
            originals.insert(path.clone(), current);
            if let Some(bytes) = next {
                policy::stage_blob(&mut staged, repo, path, bytes)?;
                let entry = staged
                    .get_path(path, 0)
                    .context("prepared entry disappeared")?;
                candidate.add(&entry)?;
            } else {
                if staged_entry.is_some() {
                    staged.remove_path(path)?;
                }
                if candidate.get_path(path, 0).is_some() {
                    candidate.remove_path(path)?;
                }
            }
        }
        let tree = candidate.write_tree_to(repo)?;
        policy::validate_tree(repo, &repo.find_tree(tree)?)?;
        let temporary = tempfile::tempdir_in(repo.path())?;
        let candidate_index = temporary.path().join("index");
        fs::write(&candidate_index, policy::index_bytes(repo, &candidate)?)?;
        let attributes = policy::git_command(repo)?
            .args([
                "check-attr",
                "--cached",
                "-z",
                "filter",
                "--",
                super::layout::PATH,
            ])
            .env("GIT_INDEX_FILE", candidate_index)
            .output()?;
        ensure!(
            attributes.status.success()
                && attributes.stdout.split(|byte| *byte == 0).nth(2) == Some(b"over"),
            "secret aggregate must retain filter=over; resolve conflicting Git attributes first"
        );
        Ok(Self {
            updates,
            originals,
            index: original_index,
            head,
            parent: parent.as_ref().map(|parent| parent.id()),
            tree,
            next_index: policy::index_bytes(repo, &staged)?,
            message,
            directory,
        })
    }

    fn validate(&self, repo: &Repository) -> Result<()> {
        ensure!(
            fs::read(repo.path().join("HEAD"))? == self.head
                && repo.head().ok().and_then(|head| head.target()) == self.parent,
            "HEAD changed while preparing secret mutation; retry"
        );
        ensure!(
            index::read_index(&repo.path().join("index"))? == self.index,
            "index changed while preparing secret mutation; retry"
        );
        for (path, bytes) in &self.originals {
            if self.directory.is_some() && path == Path::new(super::layout::PATH) {
                continue;
            }
            if let Ok(metadata) = fs::symlink_metadata(access::root(repo)?.join(path)) {
                ensure!(
                    metadata.is_file() && !metadata.is_symlink(),
                    "secret mutation input is no longer a regular file"
                );
            }
            ensure!(
                index::read_index(&access::root(repo)?.join(path))? == *bytes,
                "secret mutation input changed; inspect hook or concurrent edits and retry"
            );
        }
        if let Some((_, expected)) = &self.directory {
            ensure!(
                super::layout::legacy_files(&access::root(repo)?.join(super::layout::PATH))?
                    == *expected,
                "legacy directory changed while preparing secret mutation"
            );
        }
        Ok(())
    }

    pub(super) fn commit(self, repo: &Repository) -> Result<()> {
        self.validate(repo)?;
        let prepared = commit::PreparedCommit::prepare(repo, self.tree, &self.message)?;
        let guard = access::guard(repo, true)?;
        let index_path = repo.path().join("index");
        let mut lock = index::IndexLock::acquire(&index_path)?;
        self.validate(repo)?;
        prepared.validate(repo)?;
        let key = access::local_key(repo)?;
        let root = access::root(repo)?;
        let mut expected: BTreeMap<_, _> = self
            .updates
            .iter()
            // The directory rename owns its children; journaling them individually
            // would require traversing the new aggregate file during recovery.
            .filter(|(path, _)| {
                self.directory.is_none()
                    || path.as_path() == Path::new(super::layout::PATH)
                    || !path.starts_with(super::layout::PATH)
            })
            .map(|(path, bytes)| (root.join(path), bytes.clone()))
            .collect();
        expected.insert(index_path.clone(), Some(self.next_index.clone()));
        policy::begin_secret_commit(
            repo,
            &key,
            prepared.oid.to_string(),
            expected.clone(),
            self.directory.as_ref().map(|(backup, _)| backup.clone()),
        )?;
        let result = (|| -> Result<()> {
            if let Some((backup, _)) = &self.directory {
                fs::rename(root.join(super::layout::PATH), backup)?;
            }
            for (path, bytes) in expected.iter().filter(|(path, _)| **path != index_path) {
                if let Some(bytes) = bytes {
                    crate::crypt::transaction::worktree_write(path, bytes, None)?;
                } else if path.try_exists()? {
                    fs::remove_file(path)?;
                }
            }
            lock.replace_while_locked(&index_path, &self.next_index)?;
            // Reference hooks must fail on the pending journal rather than deadlock
            // trying to read through an exclusively held lifecycle guard.
            drop(guard);
            prepared.publish(repo)?;
            Ok(())
        })();
        drop(lock);
        if let Err(error) = result {
            policy::recover_source(repo)
                .with_context(|| format!("{error:#}; secret rollback also failed"))?;
            return Err(error);
        }
        if let Err(error) = policy::finish(repo) {
            tracing::warn!(
                "secret commit {} succeeded; recovery cleanup failed: {error:#}",
                prepared.oid
            );
        }
        commit::post_commit(repo);
        Ok(())
    }
}
