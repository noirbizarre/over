//! Prepared ordinary ingestion retained inside the crypt policy journal.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Result, ensure};

pub(crate) struct Ingestion {
    moves: Vec<(PathBuf, PathBuf, zeroize::Zeroizing<Vec<u8>>)>,
}

impl Ingestion {
    pub(crate) fn prepare(inputs: &BTreeMap<PathBuf, PathBuf>) -> Result<Self> {
        let mut moves = Vec::new();
        for (source, input) in inputs {
            let bytes = super::buffer::read_file(input)?;
            match fs::symlink_metadata(source) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_file() && !metadata.is_symlink(),
                        "source collision at '{}'",
                        source.display()
                    );
                    if same_file::is_same_file(source, input)? {
                        continue;
                    }
                    // Even equal bytes can have independent ACLs/xattrs; do not discard either inode.
                    anyhow::bail!(
                        "source '{}' already exists independently; use over crypt add for stored source files",
                        source.display()
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            moves.push((source.clone(), input.clone(), bytes));
        }
        Ok(Self { moves })
    }

    pub(crate) fn journal(
        &self,
        snapshots: &mut Vec<PathBuf>,
        expected: &mut BTreeMap<PathBuf, Option<Vec<u8>>>,
    ) -> BTreeMap<PathBuf, PathBuf> {
        let mut links = BTreeMap::new();
        for (source, input, bytes) in &self.moves {
            snapshots.extend([source.clone(), input.clone()]);
            expected.insert(source.clone(), Some(bytes.to_vec()));
            // A crash between moving the input and linking it leaves an absent input.
            expected.insert(input.clone(), None);
            links.insert(input.clone(), source.clone());
        }
        links
    }

    pub(crate) fn apply(&mut self) -> Result<()> {
        for (source, input, bytes) in &self.moves {
            ensure!(
                !input.is_symlink() && super::buffer::read_file(input)? == *bytes,
                "ingestion input changed; retry"
            );
            if let Some(parent) = source.parent() {
                fs::create_dir_all(parent)?;
            }
            if source.try_exists()? {
                anyhow::bail!("source appeared during ingestion; preserve it and retry");
            }
            fs::rename(input, source)?;
            symlink::symlink_file(source, input)?;
        }
        Ok(())
    }
}
