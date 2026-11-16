//! Explicit committed metadata for the initial checkout's smudge children only.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use super::access;

pub(crate) const ENV: &str = "OVER_CRYPT_INIT_CONTEXT";

#[derive(Deserialize, Serialize)]
pub(crate) struct Context {
    version: u32,
    git_dir: PathBuf,
    worktree: PathBuf,
    tree: String,
}

impl Context {
    pub(crate) fn new(repo: &git2::Repository, tree: git2::Oid) -> Result<Self> {
        Ok(Self {
            version: 1,
            git_dir: repo.path().canonicalize()?,
            worktree: access::root(repo)?.canonicalize()?,
            tree: tree.to_string(),
        })
    }

    pub(crate) fn encode(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub(crate) fn registry(&self, repo: &git2::Repository) -> Result<access::Registry> {
        ensure!(self.version == 1, "unsupported initial checkout context");
        ensure!(
            self.git_dir == repo.path().canonicalize()?
                && self.worktree == access::root(repo)?.canonicalize()?,
            "initial checkout context belongs to another repository"
        );
        let tree = git2::Oid::from_str(&self.tree)?;
        ensure!(
            repo.head()?.peel_to_tree()?.id() == tree,
            "HEAD changed during initial checkout"
        );
        registry(repo, tree)
    }
}

pub(crate) fn registry(repo: &git2::Repository, tree: git2::Oid) -> Result<access::Registry> {
    let tree = repo.find_tree(tree)?;
    super::access_storage::parse(&super::access_storage::tree(repo, &tree)?)?
        .context("committed repository access metadata is missing")
}

pub(crate) fn environment_registry(repo: &git2::Repository) -> Result<Option<access::Registry>> {
    std::env::var_os(ENV)
        .map(|value| {
            let value = value.to_str().context("invalid initial checkout context")?;
            serde_json::from_str::<Context>(value)?.registry(repo)
        })
        .transpose()
}

/// Git's first linked-worktree checkout shares an unlocked key, but can smudge
/// secrets before users/. Bind that narrow incomplete-index window to HEAD.
pub(crate) fn initial_checkout_registry(
    repo: &git2::Repository,
) -> Result<Option<access::Registry>> {
    if repo.path().join("index").try_exists()? || access::local_key_missing(repo)? {
        return Ok(None);
    }
    let lock = repo.path().join("index.lock");
    match std::fs::symlink_metadata(lock) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.is_symlink(),
            "initial checkout index lock must be a regular file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let tree = repo.head()?.peel_to_tree()?;
    let committed = super::access_storage::tree(repo, &tree)?;
    let working = super::access_storage::worktree(repo)?;
    ensure!(
        working
            .iter()
            .all(|(path, bytes)| committed.get(path) == Some(bytes)),
        "initial checkout access metadata diverges from HEAD"
    );
    let registry = super::access_storage::parse(&committed)?
        .context("initial checkout has no committed access metadata")?;
    ensure!(
        repo.head()?.peel_to_tree()?.id() == tree.id(),
        "HEAD changed during initial checkout"
    );
    Ok(Some(registry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn first_checkout() -> (tempfile::TempDir, git2::Repository, access::Registry) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let recipient = age::x25519::Identity::generate().to_public().to_string();
        access::add_key(&repo, "alice", "default", Some(&recipient), None, true).unwrap();
        let registry = access::load(&repo).unwrap();
        let mut index = repo.index().unwrap();
        for (path, bytes) in crate::crypt::access_storage::encode(&registry).unwrap() {
            crate::crypt::policy::stage_blob(&mut index, &repo, &path, &bytes).unwrap();
        }
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Checkout test", "checkout@example.invalid").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "bootstrap",
            &tree,
            &[],
        )
        .unwrap();
        drop(tree);
        // Model Git before any public metadata or its first index has been published.
        fs::remove_dir_all(directory.path().join(".over")).unwrap();
        assert!(!repo.path().join("index").exists());
        (directory, repo, registry)
    }

    #[test]
    fn implicit_bootstrap_requires_the_initial_index_reservation_and_an_unlocked_key() {
        let (_directory, repo, registry) = first_checkout();
        assert!(initial_checkout_registry(&repo).unwrap().is_none());
        fs::write(repo.path().join("index.lock"), b"").unwrap();
        assert_eq!(initial_checkout_registry(&repo).unwrap(), Some(registry));
        fs::write(repo.path().join("index"), b"published index").unwrap();
        assert!(initial_checkout_registry(&repo).unwrap().is_none());
        fs::remove_file(repo.path().join("index")).unwrap();
        fs::remove_file(access::private_dir(&repo).join("key")).unwrap();
        assert!(initial_checkout_registry(&repo).unwrap().is_none());
    }

    #[test]
    fn implicit_bootstrap_refuses_materialized_metadata_that_does_not_match_head() {
        let (directory, repo, registry) = first_checkout();
        fs::write(repo.path().join("index.lock"), b"").unwrap();
        let path = directory.path().join(crate::crypt::access_storage::HEADER);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let files = crate::crypt::access_storage::encode(&registry).unwrap();
        let header = &files[std::path::Path::new(crate::crypt::access_storage::HEADER)];
        fs::write(&path, header).unwrap();
        assert!(initial_checkout_registry(&repo).unwrap().is_some());
        let mut changed = header.clone();
        changed.push(b'\n');
        fs::write(&path, &changed).unwrap();
        assert!(initial_checkout_registry(&repo).is_err());
        assert_eq!(fs::read(path).unwrap(), changed);
    }
}
