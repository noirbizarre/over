//! Reconstructible artifacts for generated symlink-mode files.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use async_trait::async_trait;

use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::exec::Ctx;
use crate::plan::{ActualState, Operation, PlanStep, actual};

use super::Materializer;

pub struct RenderedMaterializer;

pub(crate) fn artifact(file: &crate::desired::content::RenderedFile) -> Result<PathBuf> {
    Ok(crate::xdg::XdgDirs::new()?
        .state_dir()
        .join("rendered")
        .join(&file.identity))
}

fn artifact_matches(
    path: &Path,
    file: &crate::desired::content::RenderedFile,
    _permissions: Option<crate::overlays::FileMode>,
) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let desired = _permissions.map_or(0o600 | (file.mode as u32 & 0o111), |mode| mode.bits());
        if metadata.permissions().mode() & 0o777 != desired & 0o777 {
            return Ok(false);
        }
    }
    Ok(fs::read(path)? == file.bytes.as_ref())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], _mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("output has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(_mode & 0o777))?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[async_trait(?Send)]
impl Materializer for RenderedMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::RenderedFile(_))
    }

    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        let MaterializationIntent::RenderedFile(file) = &entry.intent else {
            unreachable!()
        };
        let path = artifact(file)?;
        Ok(match actual::inspect(&entry.target)? {
            ActualState::Missing => Operation::Create,
            ActualState::Symlink { points_to } if points_to == path => {
                if artifact_matches(&path, file, entry.permissions)? {
                    Operation::Noop
                } else {
                    Operation::Create
                }
            }
            ActualState::Symlink { points_to } if points_to.parent() == path.parent() => {
                Operation::Create
            }
            current => Operation::Conflict { current },
        })
    }

    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let entry = step.entry.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let MaterializationIntent::RenderedFile(file) = &entry.intent else { unreachable!() };
            let path = artifact(file)?;
            crate::desired::content::validate_destination(file, &entry.target)?;
            let actual = actual::inspect(&entry.target)?;
            let managed_link = matches!(&actual, ActualState::Symlink { points_to } if points_to.parent() == path.parent());
            ensure!(matches!(actual, ActualState::Missing) || managed_link || ctx.force, "rendered destination '{}' conflicts with source template '{}'; move it aside or use --force", entry.target.display(), file.source.display());
            ensure!(!matches!(actual, ActualState::Directory), "rendered destination is a directory; move it aside");
            if !artifact_matches(&path, file, entry.permissions)? {
                atomic_write(&path, &file.bytes, entry.permissions.map_or(0o600 | (file.mode as u32 & 0o111), |mode| mode.bits()))?;
            }
            if let Some(parent) = entry.target.parent() { fs::create_dir_all(parent)?; }
            let guard = super::disposal::PathGuard::capture(&entry.target)?;
            guard.verify()?;
            if !matches!(actual, ActualState::Missing) { fs::remove_file(&entry.target)?; }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&path, &entry.target)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&path, &entry.target)?;
            Ok(())
        }).await??;
        Ok(())
    }
}
