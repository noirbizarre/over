//! Secret-specific bootstrap selection over the shared mutation publisher.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::crypt::{access, access_storage, format::RepositoryKey, mutation};

pub(super) fn mutation_guard(repo: &git2::Repository) -> Result<fs::File> {
    mutation::ownership(repo)
}

pub(super) struct Mutation {
    inner: mutation::Mutation,
    key: RepositoryKey,
}

impl Mutation {
    pub(super) fn prepare(
        repo: &git2::Repository,
        mut updates: access_storage::Delta,
        message: String,
        transitions: &[PathBuf],
    ) -> Result<Self> {
        let files = mutation::clean_metadata(repo)?;
        let head = repo
            .head()
            .ok()
            .map(|head| head.peel_to_tree())
            .transpose()?;
        let committed = head
            .as_ref()
            .map(|tree| access_storage::tree(repo, tree))
            .transpose()?
            .unwrap_or_default();
        // Initial bootstrap is inseparable from ciphertext; later pending grants
        // remain excluded by clean_metadata's complete inventory comparison.
        for (path, bytes) in files {
            if !committed.contains_key(&path) {
                updates.insert(path, Some(bytes));
            }
        }
        let mut attributes = fs::read(access::root(repo)?.join(access_storage::ATTRIBUTES))?;
        let rule = b"secrets/* filter=over diff=over -text";
        if !attributes
            .split(|byte| *byte == b'\n')
            .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == rule)
        {
            let newline = if attributes.windows(2).any(|bytes| bytes == b"\r\n") {
                b"\r\n".as_slice()
            } else {
                b"\n".as_slice()
            };
            if !attributes.ends_with(b"\n") {
                attributes.extend_from_slice(newline);
            }
            attributes.extend_from_slice(rule);
            attributes.extend_from_slice(newline);
            updates.insert(PathBuf::from(access_storage::ATTRIBUTES), Some(attributes));
        }
        let entries = updates
            .iter()
            .filter(|(path, bytes)| path.starts_with(super::layout::PATH) && bytes.is_some())
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let inner = mutation::Mutation::prepare(repo, updates, message, transitions)?;
        inner.require_protected(repo, &entries)?;
        Ok(Self {
            inner,
            key: access::local_key(repo)?,
        })
    }

    pub(super) fn commit(self, repo: &git2::Repository) -> Result<()> {
        self.inner.commit(repo, &self.key, None)
    }
}
