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
