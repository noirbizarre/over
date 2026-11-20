//! Target-only publication for immutable source consumers, without a deployment journal.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;

use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::{Context, Ctx};
use crate::overlays::{FileMode, Overlay};
use crate::plan::{ActualState, Operation, PlanStep, actual};

use super::Materializer;
use super::virtual_checkout::{safety::SourceProtection, state};

pub(crate) fn validate_source(root: &Path) -> Result<()> {
    let repo = git2::Repository::open(root).with_context(|| {
        format!(
            "cannot read source checkout '{}'; mount the complete Git checkout at --home, including its actual gitdir/common directory, and grant the lifecycle user read/traversal permissions",
            root.display()
        )
    })?;
    ensure!(
        !repo.is_bare(),
        "read-only apply requires a working checkout"
    );
    if crate::crypt::access::configured(&repo)? {
        // local_key validates existing state without creating the operation lock used by guard().
        crate::crypt::access::local_key(&repo).context(
            "read-only apply requires an already-unlocked checkout; unlock/repair it on the host and remount the complete checkout with its private Git state",
        )?;
    }
    Ok(())
}

pub(crate) fn lower(
    ctx: &Context,
    overlay: &Overlay,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let mut outputs = Vec::new();
    let mut directories = std::collections::BTreeMap::new();
    for mut entry in entries.drain(..) {
        if entry.intent.is_virtual_checkout() {
            let scope = entry.intent.checkout_scope();
            let inventory = scope
                .rendered
                .context("read-only checkout lacks source inventory")?;
            for (relative, file) in inventory.iter() {
                let logical = file.source.strip_prefix(&overlay.root)?;
                // Creating parents inside a leaf writer would lose per-source directory policies.
                for (directory, original) in relative
                    .ancestors()
                    .skip(1)
                    .zip(logical.ancestors().skip(1))
                    .filter(|(directory, _)| !directory.as_os_str().is_empty())
                {
                    let target = entry.target.join(directory);
                    directories
                        .entry(target.clone())
                        .or_insert_with(|| DesiredEntry {
                            target,
                            access: crate::target_fs::Access::new(
                                ctx,
                                Arc::new(overlay.clone()),
                                original,
                                true,
                            ),
                            provenance: Provenance::Overlay {
                                overlay: overlay.name.clone(),
                                source: overlay.root.join(original),
                            },
                            intent: MaterializationIntent::Directory,
                            permissions: None,
                        });
                }
                let access =
                    crate::target_fs::Access::new(ctx, Arc::new(overlay.clone()), logical, false);
                outputs.push(DesiredEntry {
                    target: entry.target.join(relative),
                    access,
                    provenance: Provenance::Overlay {
                        overlay: overlay.name.clone(),
                        source: file.source.clone(),
                    },
                    intent: MaterializationIntent::EphemeralFile(Arc::new(file.clone())),
                    permissions: file.permissions.or_else(|| overlay.permission_for(logical)),
                });
            }
            // Keep only the container scaffold, never the ordinary checkout initializer/transaction.
            entry.intent = MaterializationIntent::Directory;
        } else if let MaterializationIntent::RenderedFile(file) = &entry.intent {
            entry.intent = MaterializationIntent::EphemeralFile(file.clone());
        }
        outputs.push(entry);
    }
    outputs.extend(directories.into_values());
    *entries = outputs;
    validate_entries(ctx, entries)
}

pub(crate) fn validate_entries(ctx: &Context, entries: &[DesiredEntry]) -> Result<()> {
    let repo = git2::Repository::open(&ctx.repository.root)?;
    let protection = SourceProtection::new(&repo)?;
    let snapshot = state::state_file()?.inspect_blocking()?;
    for entry in entries {
        let is_link = matches!(
            entry.intent,
            MaterializationIntent::SymlinkFile { .. }
                | MaterializationIntent::SymlinkDirectory { .. }
        ) || matches!(&entry.intent, MaterializationIntent::EphemeralFile(file) if file.mode == 0o120000);
        protection.validate_read_only_target(entry, is_link)?;
        match &entry.intent {
            MaterializationIntent::SymlinkFile { source, .. } => validate_link_input(source)?,
            MaterializationIntent::SymlinkDirectory { source, .. } => {
                for item in walkdir::WalkDir::new(source).min_depth(1) {
                    let item = item?;
                    if item.file_type().is_file() {
                        validate_link_input(item.path())?;
                    }
                }
            }
            _ => {}
        }
        crate::git_exclude::validate_read_only_target_repository(entry, &repo)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Even unlinking a regular-file obstruction changes a hard-linked source inode.
            match entry.access.fs.metadata(&entry.target, false) {
                Ok(metadata) => ensure!(
                    !metadata.is_file() || metadata.nlink() == 1,
                    "read-only apply refuses hard-linked target '{}' to protect source aliases",
                    entry.target.display()
                ),
                Err(error) if crate::target_fs::is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        if let MaterializationIntent::EphemeralFile(file) = &entry.intent
            && file.protection == crate::crypt::protection::Protection::Encrypted
        {
            crate::git_exclude::ensure_sensitive_target_untracked_with(
                &entry.target,
                &entry.access.fs,
            )?;
        }
        ensure!(
            !matches!(
                entry.intent,
                MaterializationIntent::SymlinkFile {
                    link_type: crate::actions::symlink::LinkType::Hard,
                    ..
                } | MaterializationIntent::SymlinkDirectory {
                    link_type: crate::actions::symlink::LinkType::Hard,
                    ..
                }
            ),
            "read-only apply cannot create source hard links; configure a soft link instead"
        );
        for (key, record) in &snapshot.checkouts {
            let other = DesiredEntry {
                target: state::target_for_key(key)?,
                access: entry.access.clone(),
                provenance: Provenance::Overlay {
                    overlay: record.overlay.clone(),
                    source: record
                        .source_repository
                        .clone()
                        .unwrap_or_default()
                        .join(&record.managed_path),
                },
                intent: MaterializationIntent::ScopedVirtualCheckout(record.scope.clone()),
                permissions: None,
            };
            let mut candidate = entry.clone();
            candidate.target = entry.access.fs.location(&entry.target)?;
            let mut other = other;
            other.target = entry.access.fs.location(&other.target)?;
            if matches!(entry.intent, MaterializationIntent::Directory) {
                // Shared projected scaffolds remain usable, but not an exclusive managed namespace.
                ensure!(
                    record.scope.projection.is_some()
                        || !candidate.target.starts_with(&other.target),
                    "read-only destination '{}' overlaps a recorded checkout; use regular apply or unapply its old layout first",
                    entry.target.display()
                );
            }
            ensure!(
                crate::desired::ownership::conflict(&candidate, &other)?.is_none(),
                "read-only destination '{}' overlaps a recorded checkout; use regular apply or unapply its old layout first",
                entry.target.display()
            );
        }
    }
    Ok(())
}

fn validate_link_input(source: &Path) -> Result<()> {
    match std::fs::metadata(source) {
        Ok(metadata) if !metadata.is_file() => return Ok(()),
        Ok(_) => {}
        // Explicit sidecars may deliberately create dangling links for a later hook.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot inspect mounted source '{}'", source.display()));
        }
    }
    // A key alone is insufficient: links consume the host worktree, not decrypted Git blobs.
    let mut prefix = Vec::new();
    std::fs::File::open(source)
        .with_context(|| format!("cannot read mounted source '{}'", source.display()))?
        .take(crate::crypt::format::MAGIC.len() as u64)
        .read_to_end(&mut prefix)?;
    ensure!(
        !crate::crypt::format::recognized(&prefix),
        "source '{}' still contains locked ciphertext; unlock/repair its working files on the host before read-only apply",
        source.display()
    );
    Ok(())
}

pub(crate) struct EphemeralMaterializer;

#[async_trait(?Send)]
impl Materializer for EphemeralMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::EphemeralFile(_))
    }

    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        let MaterializationIntent::EphemeralFile(file) = &entry.intent else {
            unreachable!()
        };
        let current = actual::inspect_with(&entry.target, &entry.access.fs)?;
        match current {
            ActualState::Missing => Ok(Operation::Create),
            ActualState::File
                if file.mode != 0o120000
                    && entry.access.fs.read(&entry.target)?.as_slice() == file.bytes.as_ref() =>
            {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let current = FileMode::from_bits(
                        entry
                            .access
                            .fs
                            .metadata(&entry.target, false)?
                            .permissions()
                            .mode(),
                    );
                    let desired = mode(entry, file);
                    if current != desired {
                        return Ok(Operation::Repair { current, desired });
                    }
                }
                Ok(Operation::Noop)
            }
            ActualState::Symlink { ref points_to }
                if file.mode == 0o120000 && *points_to == link_path(&file.bytes)? =>
            {
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
        tokio::task::spawn_blocking(move || -> Result<()> {
            validate_entries(&ctx, std::slice::from_ref(&entry))?;
            let MaterializationIntent::EphemeralFile(file) = &entry.intent else {
                unreachable!()
            };
            let guard = super::disposal::PathGuard::capture_with(&entry.target, &entry.access.fs)?;
            let current = actual::inspect_with(&entry.target, &entry.access.fs)?;
            ensure!(
                !matches!(current, ActualState::Directory),
                "move the directory aside before deploying a read-only file"
            );
            let before = if matches!(current, ActualState::File) {
                Some(entry.access.fs.read(&entry.target)?)
            } else {
                None
            };
            let matches = before.as_deref() == Some(file.bytes.as_ref()) && file.mode != 0o120000;
            if !matches && !matches!(current, ActualState::Missing) && !ctx.force {
                ensure!(
                    !ctx.no_prompt,
                    "read-only destination '{}' differs; move it aside or use --force",
                    entry.target.display()
                );
                let _pause = crate::ui::progress::pause()?;
                if !dialoguer::Confirm::new()
                    .with_prompt(format!(
                        "Overwrite read-only apply destination '{}' ?",
                        entry.target.display()
                    ))
                    .default(false)
                    .interact()?
                {
                    return Ok(());
                }
            }
            if file.protection == crate::crypt::protection::Protection::Encrypted {
                crate::git_exclude::protect_sensitive_target(&entry)?;
            }
            guard.verify()?;
            if let Some(parent) = entry.target.parent() {
                entry.access.fs.create_dir_all(parent)?;
            }
            if file.mode == 0o120000 {
                if !matches!(current, ActualState::Missing) {
                    entry.access.fs.remove_file(&entry.target)?;
                }
                return entry
                    .access
                    .fs
                    .symlink(&link_path(&file.bytes)?, &entry.target);
            }
            ensure!(
                !matches!(current, ActualState::Symlink { .. }),
                "move the symlink aside before deploying a read-only file"
            );
            #[cfg(unix)]
            return entry.access.fs.secure_write(
                &entry.target,
                &file.bytes,
                before.as_deref(),
                mode(&entry, file).bits(),
            );
            #[cfg(not(unix))]
            {
                // TargetFs::write retains the platform's replacement/metadata safeguards.
                ensure!(
                    before
                        == match current {
                            ActualState::File => Some(entry.access.fs.read(&entry.target)?),
                            _ => None,
                        },
                    "target changed while awaiting authorization"
                );
                entry
                    .access
                    .fs
                    .write(&entry.target, &file.bytes, Some(mode(&entry, file)))
            }
        })
        .await?
    }
}

fn mode(entry: &DesiredEntry, file: &crate::desired::content::RenderedFile) -> FileMode {
    entry.permissions.unwrap_or_else(|| {
        FileMode::from_bits(
            if file.protection == crate::crypt::protection::Protection::Encrypted || file.generated
            {
                0o600 | (file.mode as u32 & 0o111)
            } else {
                file.mode as u32 & 0o777
            },
        )
    })
}

fn link_path(bytes: &[u8]) -> Result<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    Ok(std::path::PathBuf::from(std::str::from_utf8(bytes)?))
}
