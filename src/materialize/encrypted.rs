use std::fs::{self, OpenOptions};
use std::io::Write;

use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;

use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::exec::Ctx;
use crate::plan::{ActualState, Operation, PlanStep, actual};

use super::Materializer;

pub struct EncryptedFileMaterializer;

#[async_trait(?Send)]
impl Materializer for EncryptedFileMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::EncryptedFile(_))
    }

    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        let MaterializationIntent::EncryptedFile(recipe) = &entry.intent else {
            unreachable!()
        };
        match actual::inspect(&entry.target)? {
            ActualState::Missing => Ok(Operation::Create),
            ActualState::File => {
                if !recipe.crypto.available()? {
                    return Ok(Operation::NeedsUnlock);
                }
                let expected = recipe.read(false)?;
                let current = crate::secrets::SecretBytes::new(fs::read(&entry.target)?);
                if expected.expose() != current.expose() {
                    return Ok(Operation::Conflict {
                        current: ActualState::File,
                    });
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let current = crate::overlays::FileMode::from_bits(
                        fs::metadata(&entry.target)?.permissions().mode(),
                    );
                    let desired = entry
                        .permissions
                        .unwrap_or(crate::overlays::FileMode::from_bits(0o600));
                    if current.bits() != desired.bits() {
                        return Ok(Operation::Repair { current, desired });
                    }
                }
                Ok(Operation::Noop)
            }
            current => Ok(Operation::Conflict { current }),
        }
    }

    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let entry = step.entry.clone();
        let prompt = !ctx.no_prompt;
        let force = ctx.force;
        tokio::task::spawn_blocking(move || -> Result<()> {
            let MaterializationIntent::EncryptedFile(recipe) = &entry.intent else {
                unreachable!()
            };
            let plaintext = recipe.read(prompt)?;
            crate::git_exclude::ensure_sensitive_target_untracked(&entry.target)?;
            let guard = crate::materialize::disposal::PathGuard::capture(&entry.target)?;
            let state = actual::inspect(&entry.target)?;
            ensure!(
                matches!(state, ActualState::Missing | ActualState::File),
                "move the obstruction aside before deploying an encrypted file"
            );
            let before = if matches!(state, ActualState::File) {
                Some(crate::secrets::SecretBytes::new(fs::read(&entry.target)?))
            } else {
                None
            };
            let content_matches = before
                .as_ref()
                .is_some_and(|current| current.expose() == plaintext.expose());
            if before.is_some() && !content_matches && !force {
                ensure!(
                    prompt,
                    "encrypted target differs; use --force to overwrite it"
                );
                let _pause = crate::ui::progress::pause()?;
                if !dialoguer::Confirm::new()
                    .with_prompt("Overwrite encrypted file destination?")
                    .default(false)
                    .interact()
                    .context("could not confirm encrypted file overwrite")?
                {
                    return Ok(());
                }
            }
            guard.verify()?;
            ensure!(
                matches!(
                    actual::inspect(&entry.target)?,
                    ActualState::Missing | ActualState::File
                ),
                "encrypted target changed kind before writing"
            );
            if let Some(before) = &before {
                let current = crate::secrets::SecretBytes::new(fs::read(&entry.target)?);
                ensure!(
                    before.expose() == current.expose(),
                    "encrypted target changed while awaiting authorization; rerun apply"
                );
            }
            // Later apply failures must never leave a newly published secret eligible for Git staging.
            crate::git_exclude::protect_sensitive_target(&entry)?;
            let mut options = OpenOptions::new();
            options.write(true);
            if matches!(state, ActualState::Missing) {
                options.create_new(true);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&entry.target)
                .context("could not open encrypted file destination")?;
            ensure!(
                !fs::symlink_metadata(&entry.target)?
                    .file_type()
                    .is_symlink(),
                "encrypted target became a symlink"
            );
            #[cfg(windows)]
            ensure!(
                winapi_util::file::information(&file)?.number_of_links() == 1,
                "hard-linked encrypted destinations are unsupported"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                let path_metadata = fs::symlink_metadata(&entry.target)?;
                let open_metadata = file.metadata()?;
                ensure!(
                    path_metadata.dev() == open_metadata.dev()
                        && path_metadata.ino() == open_metadata.ino(),
                    "encrypted target changed before writing"
                );
                ensure!(
                    file.metadata()?.nlink() == 1,
                    "hard-linked encrypted destinations are unsupported"
                );
                file.set_permissions(fs::Permissions::from_mode(
                    entry.permissions.map_or(0o600, |mode| mode.bits()),
                ))?;
            }
            if !content_matches {
                // Only the final destination receives plaintext; permission repair must not rewrite bytes.
                file.set_len(0)?;
                file.write_all(plaintext.expose())
                    .context("could not write decrypted destination")?;
            }
            file.sync_all()
                .context("could not flush decrypted destination")?;
            Ok(())
        })
        .await?
    }
}

/// Validate every selected ciphertext before earlier entries mutate the target.
pub(crate) async fn prepare(desired: &crate::desired::DesiredTree, ctx: &Ctx) -> Result<()> {
    if ctx.dry_run {
        return Ok(());
    }
    let recipes: Vec<_> = desired
        .entries()
        .iter()
        .filter_map(|entry| {
            if let MaterializationIntent::EncryptedFile(recipe) = &entry.intent {
                Some((entry.target.clone(), recipe.clone()))
            } else {
                None
            }
        })
        .collect();
    let prompt = !ctx.no_prompt;
    tokio::task::spawn_blocking(move || -> Result<()> {
        for (target, recipe) in recipes {
            crate::git_exclude::ensure_sensitive_target_untracked(&target)?;
            ensure!(
                crate::overlays::mapping::location(&target)?
                    != crate::overlays::mapping::physical(&recipe.source)?,
                "encrypted destination overlaps its ciphertext source"
            );
            recipe.read(prompt)?;
        }
        Ok(())
    })
    .await?
}
