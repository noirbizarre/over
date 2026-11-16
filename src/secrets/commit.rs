//! Publish only the native store and its required bootstrap metadata.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

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
        ensure!(
            !staged.has_conflicts(),
            "resolve index conflicts before changing secrets"
        );
        for path in [".over/.gitattributes", ".over/crypt/access.json"] {
            let path = PathBuf::from(path);
            let bytes = fs::read(root.join(&path)).context("missing secret bootstrap metadata")?;
            let id = Oid::hash_object(git2::ObjectType::Blob, &bytes)?;
            if let Ok(old) = base.get_path(&path) {
                ensure!(
                    old.id() == id
                        && staged
                            .get_path(&path, 0)
                            .is_some_and(|entry| entry.id == id),
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
        let mut originals = BTreeMap::new();
        let mut builder = git2::build::TreeUpdateBuilder::new();
        for (path, next) in &updates {
            let absolute = root.join(path);
            if let Ok(metadata) = fs::symlink_metadata(&absolute) {
                ensure!(
                    metadata.is_file() && !metadata.is_symlink(),
                    "secret commit paths must be regular files"
                );
            }
            let current = index::read_index(&absolute)?;
            let old = base.get_path(path).ok();
            let staged_entry = staged.get_path(path, 0);
            if let Some(old) = old {
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
                builder.upsert(path.clone(), entry.id, git2::FileMode::Blob);
            } else {
                if staged_entry.is_some() {
                    staged.remove_path(path)?;
                }
                if base.get_path(path).is_ok() {
                    builder.remove(path.clone());
                }
            }
        }
        let tree = builder.create_updated(repo, &base)?;
        policy::validate_tree(repo, &repo.find_tree(tree)?)?;
        Ok(Self {
            updates,
            originals,
            index: original_index,
            head,
            parent: parent.as_ref().map(|parent| parent.id()),
            tree,
            next_index: policy::index_bytes(repo, &staged)?,
            message,
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
            ensure!(
                index::read_index(&access::root(repo)?.join(path))? == *bytes,
                "secret mutation input changed; inspect hook or concurrent edits and retry"
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
            .map(|(path, bytes)| (root.join(path), bytes.clone()))
            .collect();
        expected.insert(index_path.clone(), Some(self.next_index.clone()));
        policy::begin_commit(
            repo,
            &key,
            &expected.keys().cloned().collect::<Vec<_>>(),
            Some(prepared.oid.to_string()),
            expected.clone(),
        )?;
        let result = (|| -> Result<()> {
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
