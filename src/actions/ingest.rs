//! Symlink import preflight and declaration publication shared by add frontends.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::actions::symlink::{LinkType, SymlinkConfig};
use crate::exec::Context;
use crate::overlays::Overlay;

pub(crate) fn validate_symlink(overlay: &Overlay, path: &Path, ctx: &Context) -> Result<PathBuf> {
    let relative = overlay.resolve_mappings(ctx)?.logical_path_for(path)?;
    let source = overlay.root.join(&relative);
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    anyhow::ensure!(
        policy.includes(&source, path.is_dir()),
        "cannot ingest ignored content or control metadata '{}'",
        relative.display()
    );
    if path.is_dir() {
        policy.validate_directory_link(path, &source)?;
    }
    let desired = crate::desired::DesiredTree::build_own(ctx, overlay)?;
    anyhow::ensure!(
        !desired
            .entries()
            .iter()
            .any(|entry| entry.intent.is_virtual_checkout() && crate::stage::owns(entry, path)),
        "cannot stage a symlink in a virtual checkout"
    );
    Ok(relative)
}

pub(crate) fn referent(path: &Path) -> Result<PathBuf> {
    let target = std::fs::read_link(path)
        .map_err(|error| anyhow!("failed to read symlink {}: {error}", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("symlink has no parent"))?;
    // Moving the link text would reinterpret relative targets beside the overlay source.
    crate::overlays::mapping::absolute(&parent.join(target))
}

pub(crate) fn publish_symlink(
    overlay: &Overlay,
    relative: &Path,
    target: String,
) -> Result<PathBuf> {
    let path = overlay
        .root
        .join(format!("{}.link.toml", relative.to_string_lossy()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config = SymlinkConfig {
        target,
        r#type: LinkType::Soft,
    };
    std::fs::write(&path, toml::to_string_pretty(&config)?)?;
    Ok(path)
}
