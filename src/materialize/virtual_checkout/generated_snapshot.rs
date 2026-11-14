//! Versioned snapshot provenance and the only generated-content storage boundary.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{Commit, Oid, Repository, Tree};
use serde::{Deserialize, Serialize};

use crate::crypt::protection::Protection;
use crate::desired::content::{RenderedFile, RenderedInventory};

use super::git;

const MARKER: &str = "\n\nOver-Snapshot-Manifest: ";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub source: PathBuf,
    pub protection: Protection,
    pub source_revision: Option<String>,
    pub generated: bool,
    pub permissions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    version: u32,
    pub entries: BTreeMap<PathBuf, Entry>,
}

pub(crate) fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

impl Manifest {
    pub fn new(entries: BTreeMap<PathBuf, Entry>) -> Self {
        Self {
            version: 1,
            entries,
        }
    }

    pub fn from_inventory(repo: &Repository, inventory: &RenderedInventory) -> Result<Self> {
        let root = crate::overlays::mapping::physical(
            repo.workdir().context("snapshot source has no worktree")?,
        )?;
        let entries = inventory
            .iter()
            .map(|(path, file)| {
                // Logical source aliases must not change repository-relative persisted provenance.
                let source = crate::overlays::mapping::location(&file.source)?
                    .strip_prefix(&root)?
                    .to_path_buf();
                ensure!(
                    relative(path) && relative(&source),
                    "invalid generated snapshot provenance"
                );
                ensure!(
                    matches!(file.mode, 0o100644 | 0o100755 | 0o120000)
                        && (file.protection == Protection::Plain || file.mode != 0o120000),
                    "unsupported protected generated snapshot mode"
                );
                Ok((
                    path.clone(),
                    Entry {
                        source,
                        protection: file.protection,
                        source_revision: file.source_revision.clone(),
                        generated: file.generated,
                        permissions: file.permissions.map(|mode| mode.bits()),
                    },
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self::new(entries))
    }

    pub fn read(commit: &Commit<'_>) -> Result<Option<Self>> {
        let message = std::str::from_utf8(commit.message_bytes())?;
        let Some((_, json)) = message.split_once(MARKER) else {
            // A damaged manifest must not silently downgrade to an unprotected legacy snapshot.
            ensure!(
                !message.contains("Over-Snapshot-Manifest"),
                "malformed generated snapshot manifest"
            );
            return Ok(None);
        };
        let manifest: Self = serde_json::from_str(json.trim())?;
        ensure!(
            manifest.version == 1,
            "unsupported generated snapshot manifest version"
        );
        let leaves = git::tracked_entries(&commit.tree()?)?;
        ensure!(
            leaves.keys().eq(manifest.entries.keys()),
            "snapshot manifest does not match its tree"
        );
        for (path, entry) in &manifest.entries {
            if let Some(revision) = &entry.source_revision {
                Oid::from_str(revision)?;
            }
            ensure!(
                relative(path) && relative(&entry.source),
                "unsafe generated snapshot provenance"
            );
            ensure!(
                matches!(leaves[path].1, 0o100644 | 0o100755 | 0o120000),
                "unsupported generated snapshot mode"
            );
            ensure!(
                entry.protection == Protection::Plain || leaves[path].1 != 0o120000,
                "protected snapshot entry is not a regular file"
            );
        }
        Ok(Some(manifest))
    }

    pub fn message(&self, title: &str) -> Result<String> {
        Ok(format!("{title}{MARKER}{}", serde_json::to_string(self)?))
    }
}

pub(crate) fn write<'repo>(
    repo: &'repo Repository,
    inventory: &RenderedInventory,
) -> Result<(Tree<'repo>, Manifest)> {
    let manifest = Manifest::from_inventory(repo, inventory)?;
    let entries = inventory
        .iter()
        .map(|(path, file)| {
            let stored = file.protection.encode(repo, &file.bytes)?;
            Ok((path.clone(), (repo.blob(&stored)?, file.mode)))
        })
        .collect::<Result<_>>()?;
    Ok((git::build_content_tree(repo, &entries)?, manifest))
}

pub(crate) fn read(repo: &Repository, commit: Oid) -> Result<Option<RenderedInventory>> {
    let commit = repo.find_commit(commit)?;
    let Some(manifest) = Manifest::read(&commit)? else {
        return Ok(None);
    };
    inventory(repo, &commit, manifest).map(Some)
}

pub(crate) fn inventory(
    repo: &Repository,
    commit: &Commit<'_>,
    manifest: Manifest,
) -> Result<RenderedInventory> {
    let root = repo.workdir().context("snapshot source has no worktree")?;
    let tree = commit.tree()?;
    manifest
        .entries
        .into_iter()
        .map(|(path, entry)| {
            let leaf = tree.get_path(&path)?;
            let bytes = entry
                .protection
                .decode(repo, repo.find_blob(leaf.id())?.content())?;
            // Stored ciphertext IDs are never plaintext reconciliation fingerprints.
            let identity = Oid::hash_object(git2::ObjectType::Blob, &bytes)?.to_string();
            Ok((
                path,
                RenderedFile {
                    source: root.join(entry.source),
                    protection: entry.protection,
                    source_revision: entry.source_revision,
                    bytes: bytes.to_vec().into(),
                    identity,
                    mode: leaf.filemode(),
                    generated: entry.generated,
                    permissions: entry.permissions.map(crate::overlays::FileMode::from_bits),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("version")]
    #[case("source")]
    #[case("missing")]
    #[case("revision")]
    #[case("symlink")]
    #[case("framing")]
    fn unsafe_snapshot_manifests_are_rejected_before_content_is_materialized(
        #[case] corruption: &str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder
            .insert(
                "config",
                repo.blob(b"public test input").unwrap(),
                if corruption == "symlink" {
                    0o120000
                } else {
                    0o100644
                },
            )
            .unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let mut manifest = serde_json::json!({"version": 1, "entries": {"config": {
            "source": "source.jinja", "protection": "plain", "generated": true,
            "permissions": null, "source_revision": null,
        }}});
        match corruption {
            "version" => manifest["version"] = 2.into(),
            "source" => manifest["entries"]["config"]["source"] = "../outside".into(),
            "missing" => {
                manifest["entries"].as_object_mut().unwrap().clear();
            }
            "revision" => {
                manifest["entries"]["config"]["source_revision"] = "not-an-object-id".into()
            }
            "symlink" => manifest["entries"]["config"]["protection"] = "encrypted".into(),
            "framing" => (),
            _ => unreachable!(),
        }
        let message = if corruption == "framing" {
            "Generated checkout\nOver-Snapshot-Manifest: broken".to_owned()
        } else {
            format!("Generated checkout{MARKER}{manifest}")
        };
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        let id = repo
            .commit(None, &signature, &signature, &message, &tree, &[])
            .unwrap();
        assert!(Manifest::read(&repo.find_commit(id).unwrap()).is_err());
    }
}
