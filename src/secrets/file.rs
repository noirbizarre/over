use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};

use super::{CryptoService, SecretBytes};

/// Only ciphertext paths and redacted capability handles enter desired state.
#[derive(Debug, Clone)]
pub struct EncryptedFile {
    pub source: PathBuf,
    pub crypto: Arc<CryptoService>,
}

impl EncryptedFile {
    pub fn read(&self, prompt: bool) -> Result<SecretBytes> {
        let metadata =
            fs::symlink_metadata(&self.source).context("could not inspect encrypted source")?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "encrypted source must be a regular file"
        );
        self.crypto.decrypt(
            &fs::read(&self.source).context("could not read encrypted source")?,
            prompt,
        )
    }
}

pub fn is_encrypted(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().ends_with(b".age"))
}

pub fn reject_encrypted_directory(directory: &Path) -> Result<()> {
    // A dangling directory sidecar exposes no descendants and remains a valid link recipe.
    if directory
        .symlink_metadata()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(directory).min_depth(1) {
        let entry = entry?;
        ensure!(
            entry.file_type().is_dir() || !is_encrypted(entry.path()),
            "Age-encrypted files require ordinary file-level materialization; directory symlinks and virtual checkouts are unsupported"
        );
    }
    Ok(())
}

/// Validate the whole selection before any reverse ingestion can publish plaintext.
pub(crate) fn validate_ingestion(
    ctx: &crate::exec::Context,
    overlay: &crate::overlays::Overlay,
    paths: &[PathBuf],
) -> Result<()> {
    let tree = crate::desired::DesiredTree::build_own(ctx, overlay)?;
    for entry in tree.entries() {
        if matches!(
            entry.intent,
            crate::desired::MaterializationIntent::EncryptedFile(_)
        ) {
            for path in paths {
                let path = crate::overlays::mapping::absolute(path)?;
                let physical = if path.exists() {
                    crate::overlays::mapping::physical(&path)?
                } else {
                    crate::overlays::mapping::location(&path)?
                };
                ensure!(
                    !entry.target.starts_with(&path)
                        && !crate::overlays::mapping::location(&entry.target)?
                            .starts_with(physical),
                    "cannot ingest decrypted encrypted-file destinations; update the ciphertext source explicitly"
                );
            }
        }
    }
    Ok(())
}
