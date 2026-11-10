use std::env::current_dir;
use std::fmt;
use std::fs::{self, create_dir_all};
use std::path::{Path, PathBuf};
use std::process::Command;

use dialoguer::Select;

use anyhow::Result;
use async_trait::async_trait;
use symlink::{remove_symlink_dir, remove_symlink_file, symlink_dir, symlink_file};

use tokio::fs::rename;
use tokio::task::spawn_blocking;
use walkdir::WalkDir;

use crate::exec::{Action, Ctx};
use crate::overlays::{FileMode, Overlay};
use crate::ui;
use crate::ui::style::DialogTheme;
use crate::ui::{emojis, style};
use crate::utils::short_path;

/// Choices presented to the user when a conflict is detected during apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictChoice {
    Skip,
    Overwrite,
    Absorb,
    Diff,
}

impl ConflictChoice {
    /// All choices available for conflict resolution.
    const ALL: &[ConflictChoice] = &[
        ConflictChoice::Skip,
        ConflictChoice::Overwrite,
        ConflictChoice::Absorb,
        ConflictChoice::Diff,
    ];
}

impl fmt::Display for ConflictChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConflictChoice::Skip => write!(f, "Skip"),
            ConflictChoice::Overwrite => write!(f, "Overwrite"),
            ConflictChoice::Absorb => write!(f, "Absorb (adopt target into overlay)"),
            ConflictChoice::Diff => write!(f, "Diff (show differences, then decide)"),
        }
    }
}

/// Prompt the user to resolve a conflict between `source` (overlay) and `target` (existing).
fn prompt_conflict(source: &Path, target: &Path) -> Result<ConflictChoice> {
    let prompt = format!(
        "Conflict: {} already exists (overlay source: {})",
        style::yellow(short_path(&target.to_string_lossy())),
        style::yellow(short_path(&source.to_string_lossy())),
    );
    let selection = Select::with_theme(&DialogTheme::default())
        .with_prompt(prompt)
        .default(0)
        .items(ConflictChoice::ALL)
        .interact()
        .map_err(|e| anyhow::anyhow!("prompt failed: {}", e))?;
    Ok(ConflictChoice::ALL[selection])
}

/// Show a diff between the overlay source and the existing target using `git diff --no-index`.
fn show_diff(source: &Path, target: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("diff")
        .arg("--no-index")
        .arg("--")
        .arg(target)
        .arg(source)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run git diff: {}", e))?;
    // git diff --no-index exits with 1 when there are differences, which is expected
    if !status.success() && status.code() != Some(1) {
        anyhow::bail!("git diff exited with unexpected status: {}", status);
    }
    Ok(())
}

/// Ordinary writes must not use rename to bypass read-only permissions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparationPolicy {
    ContentWrite,
    #[cfg(unix)]
    PermissionRepair,
}

/// Update one selected file without modifying other links to its old inode.
///
/// Single-link files retain their inode (and native ACLs/streams); those writes
/// are not atomic. Linux prepares a replacement preserving owner/group/mode
/// and all ACLs/xattrs visible and accessible to the process. Other platforms
/// refuse shared inodes where native metadata cloning is unavailable.
pub(crate) fn replace_file(
    target: &Path,
    data: impl AsRef<[u8]>,
    permissions: impl FnOnce(fs::Permissions) -> fs::Permissions,
) -> Result<()> {
    replace_file_with_policy(target, data, permissions, PreparationPolicy::ContentWrite)
}

fn replace_file_with_policy(
    target: &Path,
    data: impl AsRef<[u8]>,
    permissions: impl FnOnce(fs::Permissions) -> fs::Permissions,
    policy: PreparationPolicy,
) -> Result<()> {
    let existing = match fs::symlink_metadata(target) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file(),
                "refusing to write through changed target '{}'",
                target.display()
            );
            // Atomic rename must not silently bypass a user's read-only file.
            anyhow::ensure!(
                !matches!(policy, PreparationPolicy::ContentWrite)
                    || !metadata.permissions().readonly(),
                "file '{}' is read-only",
                target.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(metadata) = &existing {
        // Keeping the inode retains native ACLs, ownership, xattrs and Windows
        // alternate streams without pretending Unix permissions describe them.
        #[cfg(unix)]
        let links = {
            use std::os::unix::fs::MetadataExt;
            metadata.nlink()
        };
        #[cfg(windows)]
        let links = winapi_util::file::information(fs::File::open(target)?)?.number_of_links();
        #[cfg(not(any(unix, windows)))]
        let links = 2;
        if links == 1 {
            let desired = permissions(metadata.permissions());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Unix writes can strip privilege metadata even while keeping
                // the inode. Refuse those files before chmod or content changes.
                anyhow::ensure!(
                    metadata.permissions().mode() & 0o6000 == 0,
                    "cannot preserve set-id metadata while writing '{}'",
                    target.display()
                );
            }
            #[cfg(target_os = "linux")]
            {
                // These attributes can be stripped or invalidated by a write;
                // inode retention alone is not sufficient to preserve them.
                for name in ["security.capability", "security.ima", "security.evm"] {
                    let value = match xattr::get(target, name) {
                        Ok(value) => value,
                        // Unsupported attributes cannot be stripped by writing;
                        // do not reject ordinary files on filesystems without them.
                        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => continue,
                        Err(error) => return Err(error.into()),
                    };
                    anyhow::ensure!(
                        value.is_none(),
                        "cannot preserve {name} while writing '{}'",
                        target.display()
                    );
                }
            }
            let mut output = fs::OpenOptions::new().write(true).open(target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Fail permission changes before writing content. Avoid chmod
                // when unnecessary because it can rewrite a native ACL mask.
                if metadata.permissions().mode() & 0o7777 != desired.mode() & 0o7777 {
                    output.set_permissions(desired.clone())?;
                }
            }
            use std::io::Write as _;
            // No truncation until preparation succeeds; subsequent I/O failure
            // can leave partial content, as with an ordinary in-place editor.
            output.write_all(data.as_ref())?;
            output.set_len(data.as_ref().len() as u64)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if output.metadata()?.permissions().mode() & 0o7777 != desired.mode() & 0o7777 {
                    output.set_permissions(desired)?;
                }
            }
            #[cfg(not(unix))]
            if output.metadata()?.permissions().readonly() != desired.readonly() {
                output.set_permissions(desired)?;
            }
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            anyhow::bail!(
                "cannot preserve native metadata while detaching hard-linked file '{}' on this platform",
                target.display()
            );
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent"))?;
    // Replacing a prepared inode avoids truncation on failure and writing
    // through hard links to unrelated files. New files retain the process umask.
    let scratch = tempfile::tempdir_in(parent)?;
    let prepared = scratch.path().join("content");
    fs::write(&prepared, data)?;
    let current = existing.as_ref().map_or_else(
        || fs::metadata(&prepared).map(|m| m.permissions()),
        |m| Ok(m.permissions()),
    )?;
    let desired = permissions(current);
    #[cfg(target_os = "linux")]
    if let Some(metadata) = &existing {
        use anyhow::Context as _;

        clone_linux_metadata(target, &prepared, metadata, desired)
            .with_context(|| format!("failed to preserve metadata for '{}'", target.display()))?;
    } else {
        fs::set_permissions(&prepared, desired)?;
    }
    #[cfg(not(target_os = "linux"))]
    fs::set_permissions(&prepared, desired)?;
    fs::rename(&prepared, target)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn clone_linux_metadata(
    source: &Path,
    prepared: &Path,
    original: &fs::Metadata,
    desired: fs::Permissions,
) -> Result<()> {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};

    let attributes = |path: &Path| -> Result<BTreeMap<std::ffi::OsString, Vec<u8>>> {
        let names = match xattr::list(path) {
            Ok(names) => names,
            // listxattr's ENOTSUP means this filesystem has no xattr support;
            // errors reading an enumerated attribute still refuse the update.
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(error.into()),
        };
        names
            .map(|name| {
                let value = xattr::get(path, &name)?.ok_or_else(|| {
                    anyhow::anyhow!("extended attribute changed during preparation: {name:?}")
                })?;
                Ok((name, value))
            })
            .collect()
    };
    let original_attributes = attributes(source)?;
    let temporary = fs::metadata(prepared)?;
    // Chown can clear set-id bits and capabilities, so do it before modes/xattrs.
    if temporary.uid() != original.uid() || temporary.gid() != original.gid() {
        chown(prepared, Some(original.uid()), Some(original.gid()))?;
    }
    fs::set_permissions(prepared, original.permissions())?;
    // The scratch directory can inherit ACLs/security labels from its parent;
    // replacing a file must not accidentally add those to its existing policy.
    for name in attributes(prepared)?.keys() {
        if !original_attributes.contains_key(name) {
            xattr::remove(prepared, name)?;
        }
    }
    for (name, value) in &original_attributes {
        xattr::set(prepared, name, value)?;
    }
    let mode_changed = desired.mode() & 0o7777 != original.permissions().mode() & 0o7777;
    if mode_changed {
        // An explicitly requested mode may intentionally change the ACL mask.
        fs::set_permissions(prepared, desired.clone())?;
    }
    let cloned = fs::metadata(prepared)?;
    anyhow::ensure!(
        cloned.uid() == original.uid()
            && cloned.gid() == original.gid()
            && cloned.permissions().mode() & 0o7777 == desired.mode() & 0o7777,
        "cannot preserve ownership/mode for '{}'",
        source.display()
    );
    let mut expected = original_attributes;
    let actual = attributes(prepared)?;
    if mode_changed
        && let Some(acl) = expected.get_mut(std::ffi::OsStr::new("system.posix_acl_access"))
    {
        apply_linux_acl_mode(acl, desired.mode())?;
    }
    anyhow::ensure!(
        actual == expected,
        "cannot preserve extended attributes for '{}'",
        source.display()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_linux_acl_mode(acl: &mut [u8], mode: u32) -> Result<()> {
    // chmod changes only the POSIX ACL owner, mask (or group), and other
    // entries. Verify those expected changes without exempting named entries.
    anyhow::ensure!(
        acl.starts_with(&2_u32.to_le_bytes()) && (acl.len() - 4).is_multiple_of(8),
        "cannot verify unsupported Linux POSIX ACL encoding"
    );
    let has_mask = acl[4..]
        .as_chunks::<8>()
        .0
        .iter()
        .any(|entry| u16::from_le_bytes([entry[0], entry[1]]) == 16);
    for entry in acl[4..].as_chunks_mut::<8>().0 {
        let access = match u16::from_le_bytes([entry[0], entry[1]]) {
            1 => Some((mode >> 6) & 7),
            4 if !has_mask => Some((mode >> 3) & 7),
            16 => Some((mode >> 3) & 7),
            32 => Some(mode & 7),
            _ => None,
        };
        if let Some(access) = access {
            entry[2..4].copy_from_slice(&(access as u16).to_le_bytes());
        }
    }
    Ok(())
}

/// Prepare before replacing so a failed copy cannot truncate the source.
fn absorb_file(source: &Path, target: &Path) -> Result<()> {
    validate_absorb(source, target)?;
    // Preparing beside the source avoids truncating it on copy failure.
    let parent = source
        .parent()
        .ok_or_else(|| anyhow::anyhow!("source has no parent"))?;
    let prepared = tempfile::NamedTempFile::new_in(parent)?;
    fs::copy(target, prepared.path())?;
    prepared.persist(source).map_err(|error| error.error)?;
    Ok(())
}

/// Absorb: recursively copy the target directory contents into the overlay source directory.
fn absorb_dir(source: &Path, target: &Path) -> Result<()> {
    validate_absorb(source, target)?;
    let parent = source
        .parent()
        .ok_or_else(|| anyhow::anyhow!("source has no parent"))?;
    let scratch = tempfile::tempdir_in(parent)?;
    let prepared = scratch.path().join("prepared");
    copy_dir_recursive(target, &prepared)?;
    let backup = scratch.path().join("original");
    let had_source = source.exists();
    if had_source {
        fs::rename(source, &backup)?;
    }
    if let Err(error) = fs::rename(&prepared, source) {
        if had_source && let Err(restore) = fs::rename(&backup, source) {
            let retained = scratch.keep();
            anyhow::bail!(
                "absorb failed: {error}; restore failed: {restore}; original retained at {}",
                retained.join("original").display()
            );
        }
        return Err(error.into());
    }
    Ok(())
}

fn validate_absorb(source: &Path, target: &Path) -> Result<()> {
    let source = crate::overlays::mapping::physical(source)?;
    // Both paths need the same Windows prefix namespace: raw canonicalization
    // adds a verbatim prefix that would otherwise hide an overlapping tree.
    let target = crate::overlays::mapping::physical(target)?;
    anyhow::ensure!(
        !source.starts_with(&target) && !target.starts_with(&source),
        "cannot absorb overlapping source and target paths"
    );
    Ok(())
}

/// Recursively copy a directory tree from `source` to `target`.
fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_symlink() {
            // Copy the link itself: following it can escape the tree or cycle.
            let destination = fs::read_link(&source_path)?;
            if source_path.is_dir() {
                symlink_dir(destination, &target_path)?;
            } else {
                symlink_file(destination, &target_path)?;
            }
        } else if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

/// Remove a target path (file, symlink, or directory).
///
/// `pub(crate)`: reused by `actions::partial::EnsurePartialBlock` (#66) for
/// its own structural-conflict resolution (a directory/symlink sitting
/// where a partial-managed file should be), rather than duplicating this
/// logic.
pub(crate) fn remove_target(target: &Path) -> Result<()> {
    if target.is_symlink() {
        // Determine if it's a file or dir symlink
        if target.is_dir() {
            remove_symlink_dir(target)?;
        } else {
            remove_symlink_file(target)?;
        }
    } else if target.is_dir() {
        fs::remove_dir_all(target)?;
    } else {
        fs::remove_file(target)?;
    }
    Ok(())
}

/// Resolve a file-level conflict using force/no_prompt flags or interactive prompt.
///
/// Returns `Ok(true)` if the caller should proceed with creating the link,
/// or `Ok(false)` if the file was skipped.
pub(crate) fn resolve_file_conflict(ctx: &Ctx, source: &Path, target: &Path) -> Result<bool> {
    resolve_conflict_with(ctx, source, target, false, prompt_conflict)
}

fn resolve_conflict_with(
    ctx: &Ctx,
    source: &Path,
    target: &Path,
    is_dir: bool,
    mut prompt: impl FnMut(&Path, &Path) -> Result<ConflictChoice>,
) -> Result<bool> {
    let source_location = crate::overlays::mapping::physical(source)?;
    let target_location = crate::overlays::mapping::location(target)?;
    // Removing the deployment must never remove its own source, even under
    // force; resolving only the target's parents keeps unlinking safe links valid.
    anyhow::ensure!(
        !source_location.starts_with(&target_location)
            && !target_location.starts_with(&source_location),
        "refusing conflict replacement with overlapping source '{}' and target '{}'",
        source.display(),
        target.display()
    );
    ensure_unprotected_target(target)?;
    if ctx.force {
        remove_target(target)?;
        return Ok(true);
    }
    if ctx.no_prompt {
        anyhow::bail!(
            "{} conflict: '{}' already exists (use --force to overwrite or run interactively to choose)",
            if is_dir { "directory" } else { "file" },
            target.display()
        );
    }
    // Interactive prompt loop
    loop {
        match prompt(source, target)? {
            ConflictChoice::Skip => return Ok(false),
            ConflictChoice::Overwrite => {
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Absorb => {
                if is_dir {
                    absorb_dir(source, target)?;
                } else {
                    absorb_file(source, target)?;
                }
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Diff => {
                show_diff(source, target)?;
                // Loop back to prompt
            }
        }
    }
}

/// Resolve a directory-level conflict using force/no_prompt flags or interactive prompt.
///
/// Returns `Ok(true)` if the caller should proceed with creating the link,
/// or `Ok(false)` if the directory was skipped.
pub(crate) fn resolve_dir_conflict(ctx: &Ctx, source: &Path, target: &Path) -> Result<bool> {
    resolve_conflict_with(ctx, source, target, true, prompt_conflict)
}

pub async fn add_file(ctx: Ctx, overlay: &Overlay, file: &PathBuf) -> Result<()> {
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    add_file_selected(ctx, overlay, file, &policy).await
}

/// Generic force/overwrite must not bypass checkout-specific disposal checks.
pub(crate) fn ensure_unprotected_target(target: &Path) -> Result<()> {
    if !target.is_symlink() && target.is_dir() {
        anyhow::ensure!(
            !target.join(".git").exists()
                && crate::materialize::virtual_checkout::state::record_for_blocking(target)?
                    .is_none(),
            "protected checkout at '{}'; use its checkout migration/unapply workflow",
            target.display()
        );
        // Recursive overwrite must also preserve checkout units nested beneath it.
        for entry in fs::read_dir(target)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                ensure_unprotected_target(&entry.path())?;
            }
        }
    }
    Ok(())
}

async fn add_file_selected(
    ctx: Ctx,
    overlay: &Overlay,
    file: &PathBuf,
    policy: &crate::overlays::ignore::SourcePolicy,
) -> Result<()> {
    let src = if file.is_relative() {
        &current_dir()?.join(file)
    } else {
        file
    };
    if ctx.debug {
        ui::info(format!("{:#?}", src)).ok();
    }
    let routing = overlay.resolve_mappings(&ctx)?;
    let root = &routing.base;
    if ctx.debug {
        ui::info(format!("{:#?}", root)).ok();
    }
    let rel_path = routing.logical_path_for(src)?;
    let target = overlay.root.join(rel_path);
    anyhow::ensure!(
        policy.includes(&target, false),
        "'{}' is ignored or is control metadata",
        src.display()
    );

    // Ensure parent directories exist in the overlay before moving the file
    if let Some(parent) = target.parent()
        && !parent.exists()
    {
        let dir_action = EnsureDir::new(parent.to_path_buf());
        if ctx.verbose || ctx.dry_run {
            ui::info(format!("{}", dir_action)).ok();
        }
        dir_action.execute(ctx.clone()).await?;
    }

    let move_action = MoveFile::new(ctx.clone(), src.clone(), target.clone());
    let link_action = EnsureLink::new(ctx.clone(), target.clone(), src.to_path_buf());

    if ctx.verbose || ctx.dry_run {
        ui::info(format!("{}", move_action)).ok();
    }
    move_action.execute(ctx.clone()).await?;

    if ctx.verbose || ctx.dry_run {
        ui::info(format!("{}", link_action)).ok();
    }
    if let Err(e) = link_action.execute(ctx.clone()).await {
        // Rollback: move file back to original location
        if !ctx.dry_run
            && let Err(rollback_err) = rename(&target, src).await
        {
            tracing::warn!(
                source = %src.display(),
                target = %target.display(),
                error = %rollback_err,
                "rollback failed: could not restore file to original location",
            );
        }
        return Err(e);
    }

    Ok(())
}

/// Add a directory to an overlay, either as a whole directory symlink (if it
/// resolves to `SymlinkDirectory` via the mode cascade — see
/// `Overlay::is_link_dir`, #113/#126) or by recursing into it and adding
/// each file individually.
pub async fn add_dir(ctx: Ctx, overlay: &Overlay, dir: &Path) -> Result<()> {
    let src = if dir.is_relative() {
        current_dir()?.join(dir)
    } else {
        dir.to_path_buf()
    };
    let routing = overlay.resolve_mappings(&ctx)?;
    let rel_path = routing.logical_path_for(&src)?;
    let policy = std::sync::Arc::new(crate::overlays::ignore::SourcePolicy::new(
        &ctx.repository.root,
        overlay,
    )?);
    anyhow::ensure!(
        policy.includes(&overlay.root.join(&rel_path), true),
        "'{}' is ignored or is control metadata",
        src.display()
    );

    if overlay.is_link_dir(&rel_path) {
        policy.validate_directory_link(&src, &overlay.root.join(&rel_path))?;
        // Symlink the entire directory as a unit
        let target = overlay.root.join(&rel_path);

        // Ensure parent directories exist in the overlay
        if let Some(parent) = target.parent()
            && !parent.exists()
        {
            let dir_action = EnsureDir::new(parent.to_path_buf());
            if ctx.verbose || ctx.dry_run {
                ui::info(format!("{}", dir_action)).ok();
            }
            dir_action.execute(ctx.clone()).await?;
        }

        let move_action = MoveFile::new(ctx.clone(), src.clone(), target.clone());
        let link_action = EnsureDirLink::new(ctx.clone(), target, src);

        if ctx.verbose || ctx.dry_run {
            ui::info(format!("{}", move_action)).ok();
        }
        move_action.execute(ctx.clone()).await?;

        if ctx.verbose || ctx.dry_run {
            ui::info(format!("{}", link_action)).ok();
        }
        link_action.execute(ctx.clone()).await?;
    } else {
        // Recurse into the directory and add each file individually.
        // `WalkDir` is synchronous disk I/O, hence `spawn_blocking` rather
        // than walking directly inside this `async fn`.
        let walk_src = src.clone();
        let walk_overlay = overlay.clone();
        let walk_routing = routing.clone();
        let walk_policy = policy.clone();
        let files: Vec<PathBuf> = spawn_blocking(move || {
            WalkDir::new(&walk_src)
                .min_depth(1)
                .into_iter()
                .filter_entry(|entry| {
                    !entry.path().starts_with(&walk_overlay.root)
                        && walk_routing
                            .logical_path_for(entry.path())
                            .is_ok_and(|logical| {
                                walk_policy.includes(
                                    &walk_overlay.root.join(logical),
                                    entry.file_type().is_dir(),
                                )
                            })
                })
                .filter_map(|entry| match entry {
                    Ok(e) => Some(e),
                    Err(e) => {
                        tracing::warn!("skipping entry due to error: {}", e);
                        None
                    }
                })
                .filter(|e| e.file_type().is_file())
                .filter(|e| {
                    let rel = match walk_routing.logical_path_for(e.path()) {
                        Ok(r) => r,
                        Err(_) => return false,
                    };
                    !walk_overlay.is_excluded(&rel)
                })
                .map(|e| e.path().to_path_buf())
                .collect()
        })
        .await?;

        for file in files {
            add_file_selected(ctx.clone(), overlay, &file, &policy).await?;
        }
    }

    Ok(())
}

/// Add a path (file or directory) to an overlay.
pub async fn add_path(ctx: Ctx, overlay: &Overlay, path: &Path) -> Result<()> {
    let src = if path.is_relative() {
        current_dir()?.join(path)
    } else {
        path.to_path_buf()
    };

    if src.is_dir() {
        add_dir(ctx, overlay, &src).await
    } else if src.is_file() {
        add_file(ctx, overlay, &src).await
    } else {
        Err(anyhow::anyhow!(
            "{} does not exist or is not a file/directory",
            src.display(),
        ))
    }
}

pub struct EnsureLink {
    pub ctx: Ctx,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl EnsureLink {
    pub fn new(ctx: Ctx, source: PathBuf, target: PathBuf) -> Self {
        Self {
            ctx,
            source,
            target,
        }
    }
}

impl fmt::Display for EnsureLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && overlay.map.is_empty()
            && let Ok(rel_path) = self.source.strip_prefix(&overlay.root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .target
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.target.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::LINK,
                style::white("link:"),
                style::white("{"),
                short_path(&overlay.root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::LINK,
            style::white("link:"),
            self.source.display(),
            self.target.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureLink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let source = self.source.clone();
        let target = self.target.clone();
        let ctx2 = ctx.clone();
        spawn_blocking(move || {
            if target.exists() || target.is_symlink() {
                if target.is_symlink() {
                    let src = fs::read_link(target.as_path())?;
                    if src == source {
                        // Already correctly linked, no conflict
                        return Ok(());
                    }
                }
                // Conflict: target exists and is not the correct symlink
                if !resolve_file_conflict(&ctx2, &source, &target)? {
                    return Ok(()); // Skipped
                }
            }
            if let Some(parent) = target.parent() {
                create_dir_all(parent)?;
            }
            symlink_file(source.as_path(), target.as_path())?;
            Ok(())
        })
        .await?
    }
}

pub struct EnsureDir {
    pub path: PathBuf,
    // pub target: PathBuf,
}

impl EnsureDir {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl fmt::Display for EnsureDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}",
            emojis::DIRECTORY,
            style::white("create directory:"),
            self.path.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureDir {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if !ctx.dry_run {
            let path = self.path.clone();
            spawn_blocking(move || -> Result<()> {
                if path.is_symlink() || (path.exists() && !path.is_dir()) {
                    // Stop on decline so children cannot follow this parent.
                    ensure_unprotected_target(&path)?;
                    if !ctx.force {
                        anyhow::ensure!(
                            !ctx.no_prompt,
                            "directory conflict at '{}' (use --force to replace)",
                            path.display()
                        );
                        let overwrite = dialoguer::Confirm::new()
                            .with_prompt(format!(
                                "Replace directory scaffold conflict at '{}' ?",
                                path.display()
                            ))
                            .default(false)
                            .interact()?;
                        anyhow::ensure!(
                            overwrite,
                            "directory conflict declined at '{}'",
                            path.display()
                        );
                    }
                    remove_target(&path)?;
                }
                create_dir_all(path.as_path())?;
                Ok(())
            })
            .await??;
        }
        Ok(())
    }
}

/// Enforce a declared permission mode on an existing target (#65) — the
/// materialization for `Operation::Repair`, when an entry's content/kind
/// already matches but its mode doesn't. Never creates or removes
/// anything; the target must already exist.
pub struct SetPermissions {
    pub path: PathBuf,
    pub mode: FileMode,
}

impl SetPermissions {
    pub fn new(path: PathBuf, mode: FileMode) -> Self {
        Self { path, mode }
    }
}

impl fmt::Display for SetPermissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} ({})",
            emojis::LOCK,
            style::white("set permissions:"),
            short_path(&self.path.to_string_lossy()),
            self.mode,
        )
    }
}

#[async_trait]
impl Action for SetPermissions {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = self.path.clone();
            let mode = self.mode.bits();
            spawn_blocking(move || -> Result<()> {
                // A chmod of a shared inode would affect paths outside ownership.
                let metadata = fs::symlink_metadata(&path)?;
                anyhow::ensure!(metadata.is_file(), "permission target changed type");
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 {
                    replace_file_with_policy(
                        &path,
                        fs::read(&path)?,
                        |_| fs::Permissions::from_mode(mode),
                        PreparationPolicy::PermissionRepair,
                    )?;
                } else {
                    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
                }
                Ok(())
            })
            .await??;
        }
        // Permission bits aren't a meaningful concept to enforce here on
        // non-unix platforms — nothing to do (see ADR-020).
        #[cfg(not(unix))]
        {
            tracing::debug!(path = %self.path.display(), "permission metadata is not enforced on non-unix platforms");
        }
        Ok(())
    }
}

pub struct EnsureDirLink {
    pub ctx: Ctx,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl EnsureDirLink {
    pub fn new(ctx: Ctx, source: PathBuf, target: PathBuf) -> Self {
        Self {
            ctx,
            source,
            target,
        }
    }
}

impl fmt::Display for EnsureDirLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && overlay.map.is_empty()
            && let Ok(rel_path) = self.source.strip_prefix(&overlay.root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .target
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.target.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::LINK,
                style::white("link dir:"),
                style::white("{"),
                short_path(&overlay.root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::LINK,
            style::white("link dir:"),
            self.source.display(),
            self.target.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureDirLink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let source = self.source.clone();
        let target = self.target.clone();
        let ctx2 = ctx.clone();
        spawn_blocking(move || {
            if target.exists() || target.is_symlink() {
                if target.is_symlink() {
                    let src = fs::read_link(target.as_path())?;
                    if src == source {
                        // Already correctly linked, no conflict
                        return Ok(());
                    }
                }
                // Conflict: target exists and is not the correct symlink
                if !resolve_dir_conflict(&ctx2, &source, &target)? {
                    return Ok(()); // Skipped
                }
            }
            if let Some(parent) = target.parent() {
                create_dir_all(parent)?;
            }
            symlink_dir(source.as_path(), target.as_path())?;
            Ok(())
        })
        .await?
    }
}

pub struct MoveFile {
    pub ctx: Ctx,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl MoveFile {
    pub fn new(ctx: Ctx, source: PathBuf, target: PathBuf) -> Self {
        Self {
            ctx,
            source,
            target,
        }
    }
}

impl fmt::Display for MoveFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && overlay.map.is_empty()
            && let Ok(source_root) = overlay.resolve_target(&self.ctx)
            && let Ok(rel_path) = self.source.strip_prefix(&source_root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .target
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.target.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::MOVE_FILE,
                style::white("move file:"),
                style::white("{"),
                short_path(&source_root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::MOVE_FILE,
            style::white("move file:"),
            self.source.display(),
            self.target.display(),
        )
    }
}

#[async_trait]
impl Action for MoveFile {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if !ctx.dry_run {
            rename(&self.source, &self.target).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use std::fs;

    #[test]
    #[cfg(target_os = "linux")]
    fn replacing_a_file_preserves_content_behind_unowned_hard_links() {
        let td = TempDir::new().unwrap();
        let owned = td.path().join("owned");
        let unowned = td.path().join("unowned");
        fs::write(&unowned, "private").unwrap();
        fs::hard_link(&unowned, &owned).unwrap();
        replace_file(&owned, "updated", |permissions| permissions).unwrap();
        assert_eq!(fs::read_to_string(owned).unwrap(), "updated");
        assert_eq!(fs::read_to_string(unowned).unwrap(), "private");
    }

    #[rstest::rstest]
    fn ordinary_content_writes_refuse_readonly_files_before_changing_any_link(
        #[values(false, true)] hardlinked: bool,
    ) {
        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        let alias = td.path().join("alias");
        fs::write(&target, "private").unwrap();
        if hardlinked {
            fs::hard_link(&target, &alias).unwrap();
        }
        let original = fs::metadata(&target).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&target, readonly).unwrap();
        assert!(replace_file(&target, "changed", |permissions| permissions).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"private");
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
        if hardlinked {
            assert_eq!(fs::read(&alias).unwrap(), b"private");
            assert!(fs::metadata(&alias).unwrap().permissions().readonly());
        }
        // Windows cannot remove read-only test files during TempDir cleanup.
        fs::set_permissions(&target, original).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn permission_repair_detaches_a_readonly_hardlink_without_changing_its_alias() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        let alias = td.path().join("alias");
        fs::write(&alias, "private").unwrap();
        fs::set_permissions(&alias, fs::Permissions::from_mode(0o444)).unwrap();
        fs::hard_link(&alias, &target).unwrap();
        let original = fs::metadata(&alias).unwrap();
        SetPermissions::new(target.clone(), FileMode::parse("644").unwrap())
            .execute(Context::builder().build())
            .await
            .unwrap();
        let repaired = fs::metadata(&target).unwrap();
        let untouched = fs::metadata(&alias).unwrap();
        assert_eq!(repaired.permissions().mode() & 0o7777, 0o644);
        assert_eq!(untouched.permissions().mode() & 0o7777, 0o444);
        assert_eq!(untouched.ino(), original.ino());
        assert_ne!(repaired.ino(), untouched.ino());
        assert_eq!(
            (repaired.uid(), repaired.gid()),
            (original.uid(), original.gid())
        );
        assert_eq!(fs::read(target).unwrap(), b"private");
        assert_eq!(fs::read(alias).unwrap(), b"private");
    }

    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    fn replacement_preserves_acl_xattrs_and_ownership_on_every_surviving_link(
        #[values(false, true)] hardlinked: bool,
        #[values(false, true)] change_mode: bool,
    ) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        let alias = td.path().join("alias");
        fs::write(&target, "private").unwrap();
        xattr::set(&target, "user.over-regression", b"retain me").unwrap();
        // Linux POSIX ACL version 2, including a named user: mode bits alone
        // cannot reconstruct this policy. Avoid an external setfacl dependency.
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, access, id) in [
            (1_u16, 6_u16, u32::MAX),
            (2, 4, 12345),
            (4, 4, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(access.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        xattr::set(&target, "system.posix_acl_access", &acl).unwrap();
        if hardlinked {
            fs::hard_link(&target, &alias).unwrap();
        }
        let original = fs::metadata(&target).unwrap();
        replace_file(&target, "updated", |permissions| {
            if change_mode {
                fs::Permissions::from_mode(0o700)
            } else {
                permissions
            }
        })
        .unwrap();
        for path in std::iter::once(&target).chain(hardlinked.then_some(&alias)) {
            let current = fs::metadata(path).unwrap();
            assert_eq!(
                (current.uid(), current.gid()),
                (original.uid(), original.gid())
            );
            let mut expected_acl = acl.clone();
            let expected_mode = if change_mode && path == &target {
                // The owner gains execute and the mask loses group access;
                // named-user permissions and group-object entries must survive.
                expected_acl[6..8].copy_from_slice(&7_u16.to_le_bytes());
                expected_acl[30..32].copy_from_slice(&0_u16.to_le_bytes());
                (original.mode() & !0o7777) | 0o700
            } else {
                original.mode()
            };
            assert_eq!(current.mode(), expected_mode);
            assert_eq!(
                xattr::get(path, "system.posix_acl_access").unwrap(),
                Some(expected_acl)
            );
            assert_eq!(
                xattr::get(path, "user.over-regression").unwrap(),
                Some(b"retain me".to_vec())
            );
        }
        if !hardlinked {
            assert_eq!(fs::metadata(&target).unwrap().ino(), original.ino());
        }
        assert_eq!(fs::read(target).unwrap(), b"updated");
        if hardlinked {
            assert_eq!(fs::metadata(&alias).unwrap().ino(), original.ino());
            assert_eq!(fs::read(alias).unwrap(), b"private");
        }
    }

    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    fn writes_refuse_privilege_metadata_that_the_kernel_could_strip(
        #[values(0o2640, 0o4640)] mode: u32,
    ) {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        fs::write(&target, "original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        assert!(replace_file(&target, "changed", |p| p).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            mode
        );
    }

    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    fn replacement_preserves_a_nondefault_group_when_chown_is_supported(
        #[values(false, true)] hardlinked: bool,
    ) {
        use std::os::unix::fs::{MetadataExt, chown};

        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        fs::write(&target, "original").unwrap();
        let default_group = fs::metadata(&target).unwrap().gid();
        let other_group = if default_group == 1 { 0 } else { 1 };
        if let Err(error) = chown(&target, None, Some(other_group)) {
            // An unprivileged runner may not belong to another group; only
            // that capability-dependent case is skipped, not preservation errors.
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            eprintln!("alternate-group fixture unavailable: {error}");
            return;
        }
        let alias = td.path().join("alias");
        if hardlinked {
            fs::hard_link(&target, &alias).unwrap();
        }
        replace_file(&target, "updated", |permissions| permissions).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().gid(), other_group);
        assert_eq!(fs::read(target).unwrap(), b"updated");
        if hardlinked {
            assert_eq!(fs::metadata(&alias).unwrap().gid(), other_group);
            assert_eq!(fs::read(alias).unwrap(), b"original");
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_native_metadata_cloning_refuses_to_detach_a_shared_inode() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        let alias = td.path().join("alias");
        fs::write(&target, "private").unwrap();
        fs::hard_link(&target, &alias).unwrap();
        assert!(replace_file(&target, "changed", |permissions| permissions).is_err());
        assert_eq!(fs::read(target).unwrap(), b"private");
        assert_eq!(fs::read(alias).unwrap(), b"private");
    }

    #[rstest::rstest]
    fn interactive_conflict_choices_are_the_only_authority_for_loss(
        #[values(false, true)] directory: bool,
        #[values(
            ConflictChoice::Skip,
            ConflictChoice::Overwrite,
            ConflictChoice::Absorb
        )]
        choice: ConflictChoice,
    ) {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = td.path().join("target");
        let (source_content, target_content) = if directory {
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&target).unwrap();
            (source.join("file"), target.join("file"))
        } else {
            (source.clone(), target.clone())
        };
        fs::write(&source_content, "overlay").unwrap();
        fs::write(&target_content, "private").unwrap();
        let ctx = Context::builder().build();
        let proceed =
            resolve_conflict_with(&ctx, &source, &target, directory, |_, _| Ok(choice)).unwrap();
        assert_eq!(proceed, choice != ConflictChoice::Skip);
        assert_eq!(
            fs::read_to_string(&source_content).unwrap(),
            if choice == ConflictChoice::Absorb {
                "private"
            } else {
                "overlay"
            }
        );
        if choice == ConflictChoice::Skip {
            assert_eq!(fs::read_to_string(target_content).unwrap(), "private");
        } else {
            assert!(!target.exists());
        }
    }

    #[test]
    fn showing_a_diff_then_skipping_preserves_both_inputs() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = td.path().join("target");
        fs::write(&source, "overlay").unwrap();
        fs::write(&target, "private").unwrap();
        let mut choices = [ConflictChoice::Diff, ConflictChoice::Skip].into_iter();
        assert!(
            !resolve_conflict_with(
                &Context::builder().build(),
                &source,
                &target,
                false,
                |_, _| Ok(choices.next().unwrap())
            )
            .unwrap()
        );
        assert_eq!(fs::read_to_string(source).unwrap(), "overlay");
        assert_eq!(fs::read_to_string(target).unwrap(), "private");
    }

    #[cfg(unix)]
    #[test]
    fn absorb_copies_directory_links_without_following_cycles() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = td.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        symlink_dir(&target, target.join("cycle")).unwrap();
        absorb_dir(&source, &target).unwrap();
        assert_eq!(fs::read_link(source.join("cycle")).unwrap(), target);
    }

    #[test]
    fn absorb_rejects_overlapping_trees_before_modifying_the_source() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = source.join("child");
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("keep"), "private").unwrap();
        assert!(absorb_dir(&source, &target).is_err());
        assert_eq!(fs::read_to_string(source.join("keep")).unwrap(), "private");
    }

    #[cfg(windows)]
    #[rstest::rstest]
    fn absorb_rejects_mixed_windows_prefixes_before_modifying_the_source(
        #[values(false, true)] verbatim_source: bool,
    ) {
        let td = TempDir::new().unwrap();
        let source = td.path().join("source");
        let target = source.join("child");
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("keep"), "private").unwrap();
        let (source_alias, target_alias) = if verbatim_source {
            (source.canonicalize().unwrap(), target.clone())
        } else {
            (source.clone(), target.canonicalize().unwrap())
        };
        assert!(absorb_dir(&source_alias, &target_alias).is_err());
        assert_eq!(fs::read_to_string(source.join("keep")).unwrap(), "private");
        assert!(target.is_dir());
    }

    #[test]
    fn force_cannot_delete_the_source_inside_a_conflicting_target() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("target");
        let source = target.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("private"), "only copy").unwrap();
        let ctx = Context::builder().force(true).build();
        assert!(resolve_dir_conflict(&ctx, &source, &target).is_err());
        assert_eq!(
            fs::read_to_string(source.join("private")).unwrap(),
            "only copy"
        );
    }

    /// Install a tracing subscriber so `tracing::warn!` etc. bodies are executed during tests.
    fn init_test_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::TRACE)
            .try_init();
    }

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        let mut builder = Context::builder()
            .verbose(true)
            .force(true)
            .root(root)
            .repository(repo);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    #[tokio::test]
    async fn ensure_dir_creates_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let dir_path = td.path().join("new_dir");
        let action = EnsureDir::new(dir_path.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(dir_path.exists(), "directory should be created");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn set_permissions_changes_mode() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let file = td.child("file.txt");
        file.write_str("content").unwrap();
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let action =
            SetPermissions::new(file.path().to_path_buf(), FileMode::parse("600").unwrap());
        let ctx = Context::builder().build();
        action.execute(ctx).await.unwrap();

        let mode = fs::metadata(file.path()).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn set_permissions_dry_run_does_not_touch_filesystem() {
        use std::os::unix::fs::PermissionsExt;

        let td = TempDir::new().unwrap();
        let file = td.child("file.txt");
        file.write_str("content").unwrap();
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let action =
            SetPermissions::new(file.path().to_path_buf(), FileMode::parse("600").unwrap());
        let ctx = Context::builder().dry_run(true).build();
        action.execute(ctx).await.unwrap();

        let mode = fs::metadata(file.path()).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644, "dry_run must never touch the filesystem");
    }

    #[test]
    fn set_permissions_display() {
        let action = SetPermissions::new(
            PathBuf::from("/tmp/file.txt"),
            FileMode::parse("600").unwrap(),
        );
        let s = format!("{action}");
        assert!(s.contains("set permissions:"));
        assert!(s.contains("600"));
    }

    #[tokio::test]
    async fn move_file_moves() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_move");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_move").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let original = td.child("file.txt");
        original.write_str("content").unwrap();
        let dst = overlay.root.join("file.txt");
        let action = MoveFile::new(c.clone(), original.path().to_path_buf(), dst.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(dst.exists(), "file moved into overlay root");
        assert!(!original.path().exists(), "original should be moved away");
    }

    #[tokio::test]
    async fn ensure_link_creates_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_link");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_link").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_file = overlay.root.join("inner.txt");
        fs::write(&src_file, "hello").unwrap();
        let target = td.path().join("inner.txt");
        let action = EnsureLink::new(c.clone(), src_file.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        let link_target = fs::read_link(&target).unwrap();
        assert_eq!(link_target, src_file);
    }

    #[tokio::test]
    async fn add_file_errors_when_outside_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_add_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        // outside file
        let outside_dir = assert_fs::TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("ext.txt");
        fs::write(&outside_file, "ext").unwrap();
        let res = add_file(c.clone(), &overlay, &outside_file).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn add_file_creates_parent_dirs_in_overlay() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_nested").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a nested file: <root>/.config/app/settings.conf
        let nested_dir = td.path().join(".config").join("app");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_file = nested_dir.join("settings.conf");
        fs::write(&nested_file, "key=value").unwrap();

        let res = add_file(c.clone(), &overlay, &nested_file).await;
        assert!(res.is_ok(), "add_file should succeed: {:?}", res.err());

        // Check the file was moved to the overlay
        let moved_path = overlay.root.join(".config/app/settings.conf");
        assert!(moved_path.exists(), "file should exist in overlay");
        assert_eq!(fs::read_to_string(&moved_path).unwrap(), "key=value");

        // Check symlink was created at original location
        assert!(nested_file.is_symlink(), "original should be a symlink");
        assert_eq!(fs::read_link(&nested_file).unwrap(), moved_path);
    }

    #[tokio::test]
    async fn add_dir_recursively_adds_files() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a directory with files: <root>/mydir/{a.txt, sub/b.txt}
        let dir = td.path().join("mydir");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "aaa").unwrap();
        fs::write(dir.join("sub").join("b.txt"), "bbb").unwrap();

        let res = add_dir(c.clone(), &overlay, &dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        // Both files should be moved to overlay and symlinked back
        let moved_a = overlay.root.join("mydir/a.txt");
        let moved_b = overlay.root.join("mydir/sub/b.txt");
        assert!(moved_a.exists(), "a.txt should be in overlay");
        assert!(moved_b.exists(), "b.txt should be in overlay");
        assert_eq!(fs::read_to_string(&moved_a).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(&moved_b).unwrap(), "bbb");

        // Original locations should be symlinks
        let orig_a = dir.join("a.txt");
        let orig_b = dir.join("sub").join("b.txt");
        assert!(orig_a.is_symlink(), "a.txt should be a symlink");
        assert!(orig_b.is_symlink(), "b.txt should be a symlink");
    }

    #[tokio::test]
    async fn add_dir_as_link_dir_symlinks_whole_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_linkdir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[[rules]]\npath = 'mydir'\nmode = 'symlink-directory'")
            .unwrap();
        let overlay = repo.get("ov_linkdir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a directory with files
        let dir = td.path().join("mydir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file.txt"), "content").unwrap();

        let res = add_dir(c.clone(), &overlay, &dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        // The whole directory should be moved to the overlay
        let moved_dir = overlay.root.join("mydir");
        assert!(moved_dir.is_dir(), "mydir should exist as dir in overlay");
        assert!(
            moved_dir.join("file.txt").exists(),
            "file.txt should be in overlay dir"
        );

        // Original location should be a symlink to the overlay directory
        assert!(dir.is_symlink(), "original dir should be a symlink");
        assert_eq!(fs::read_link(&dir).unwrap(), moved_dir);
    }

    #[tokio::test]
    async fn add_path_dispatches_file() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_file");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_file").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let file = td.path().join("pathfile.txt");
        fs::write(&file, "data").unwrap();

        let res = add_path(c.clone(), &overlay, &file).await;
        assert!(
            res.is_ok(),
            "add_path for file should succeed: {:?}",
            res.err()
        );

        let moved = overlay.root.join("pathfile.txt");
        assert!(moved.exists(), "file should be in overlay");
        assert!(file.is_symlink(), "original should be a symlink");
    }

    #[tokio::test]
    async fn add_path_dispatches_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_dir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_dir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let dir = td.path().join("pathdir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.txt"), "xxx").unwrap();

        let res = add_path(c.clone(), &overlay, &dir).await;
        assert!(
            res.is_ok(),
            "add_path for dir should succeed: {:?}",
            res.err()
        );

        let moved = overlay.root.join("pathdir/x.txt");
        assert!(moved.exists(), "file should be in overlay");
    }

    #[tokio::test]
    async fn add_path_nonexistent_errors() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let nonexistent = td.path().join("does_not_exist.txt");
        let res = add_path(c.clone(), &overlay, &nonexistent).await;
        assert!(res.is_err(), "add_path for nonexistent should error");
    }

    // ── ConflictChoice Display ──────────────────────────────────────────

    #[test]
    fn conflict_choice_display() {
        assert_eq!(format!("{}", ConflictChoice::Skip), "Skip");
        assert_eq!(format!("{}", ConflictChoice::Overwrite), "Overwrite");
        assert_eq!(
            format!("{}", ConflictChoice::Absorb),
            "Absorb (adopt target into overlay)"
        );
        assert_eq!(
            format!("{}", ConflictChoice::Diff),
            "Diff (show differences, then decide)"
        );
    }

    #[test]
    fn conflict_choice_all_has_four_variants() {
        assert_eq!(ConflictChoice::ALL.len(), 4);
    }

    // ── copy_dir_recursive ──────────────────────────────────────────────

    #[test]
    fn copy_dir_recursive_copies_nested_tree() {
        let td = TempDir::new().unwrap();
        let src = td.path().join("src_dir");
        fs::create_dir_all(src.join("sub1/sub2")).unwrap();
        fs::write(src.join("a.txt"), "aaa").unwrap();
        fs::write(src.join("sub1/b.txt"), "bbb").unwrap();
        fs::write(src.join("sub1/sub2/c.txt"), "ccc").unwrap();

        let dst = td.path().join("dst_dir");
        copy_dir_recursive(&src, &dst).unwrap();

        assert!(dst.join("a.txt").exists());
        assert!(dst.join("sub1/b.txt").exists());
        assert!(dst.join("sub1/sub2/c.txt").exists());
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "aaa");
        assert_eq!(
            fs::read_to_string(dst.join("sub1/sub2/c.txt")).unwrap(),
            "ccc"
        );
    }

    #[test]
    fn copy_dir_recursive_empty_dir() {
        let td = TempDir::new().unwrap();
        let src = td.path().join("empty_src");
        fs::create_dir_all(&src).unwrap();

        let dst = td.path().join("empty_dst");
        copy_dir_recursive(&src, &dst).unwrap();

        assert!(dst.exists());
        assert!(dst.is_dir());
    }

    // ── remove_target ────────────────────────────────────────────────────

    #[test]
    fn remove_target_removes_file() {
        let td = TempDir::new().unwrap();
        let file = td.path().join("removable.txt");
        fs::write(&file, "content").unwrap();
        assert!(file.exists());
        remove_target(&file).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn remove_target_removes_directory() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("removable_dir");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/file.txt"), "content").unwrap();
        assert!(dir.exists());
        remove_target(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_target_removes_file_symlink() {
        let td = TempDir::new().unwrap();
        let real_file = td.path().join("real.txt");
        fs::write(&real_file, "content").unwrap();
        let link = td.path().join("link.txt");
        symlink::symlink_file(&real_file, &link).unwrap();
        assert!(link.is_symlink());
        remove_target(&link).unwrap();
        assert!(!link.exists());
        // Original should still exist
        assert!(real_file.exists());
    }

    #[test]
    fn remove_target_removes_dir_symlink() {
        let td = TempDir::new().unwrap();
        let real_dir = td.path().join("real_dir");
        fs::create_dir_all(&real_dir).unwrap();
        let link = td.path().join("dir_link");
        symlink::symlink_dir(&real_dir, &link).unwrap();
        assert!(link.is_symlink());
        remove_target(&link).unwrap();
        assert!(!link.exists() || !link.is_symlink());
        // Original should still exist
        assert!(real_dir.exists());
    }

    // ── absorb_file / absorb_dir ─────────────────────────────────────────

    #[test]
    fn absorb_file_copies_target_to_source() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("overlay_file.txt");
        fs::write(&source, "old content").unwrap();
        let target = td.path().join("target_file.txt");
        fs::write(&target, "new content").unwrap();

        absorb_file(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "new content");
    }

    #[test]
    fn absorb_dir_replaces_source_with_target() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("overlay_dir");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("old.txt"), "old").unwrap();

        let target = td.path().join("target_dir");
        fs::create_dir_all(target.join("sub")).unwrap();
        fs::write(target.join("new.txt"), "new").unwrap();
        fs::write(target.join("sub/deep.txt"), "deep").unwrap();

        absorb_dir(&source, &target).unwrap();
        assert!(source.join("new.txt").exists());
        assert!(source.join("sub/deep.txt").exists());
        assert!(!source.join("old.txt").exists());
    }

    #[test]
    fn absorb_dir_source_does_not_exist() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("nonexistent_dir");
        let target = td.path().join("target_dir");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("file.txt"), "data").unwrap();

        absorb_dir(&source, &target).unwrap();
        assert!(source.join("file.txt").exists());
    }

    // ── resolve_file_conflict / resolve_dir_conflict ─────────────────────

    #[test]
    fn resolve_file_conflict_force_removes_target() {
        let td = TempDir::new().unwrap();
        let (_, repo) = repo_and_root();
        let c = ctx(td.path().to_path_buf(), repo, None);
        // c has force=true

        let source = td.path().join("source.txt");
        fs::write(&source, "source").unwrap();
        let target = td.path().join("target.txt");
        fs::write(&target, "target").unwrap();

        let result = resolve_file_conflict(&c, &source, &target).unwrap();
        assert!(result, "force should resolve conflict by removing target");
        assert!(!target.exists(), "target should be removed");
    }

    #[test]
    fn resolve_dir_conflict_force_removes_target() {
        let td = TempDir::new().unwrap();
        let (_, repo) = repo_and_root();
        let c = ctx(td.path().to_path_buf(), repo, None);

        let source = td.path().join("source_dir");
        fs::create_dir_all(&source).unwrap();
        let target = td.path().join("target_dir");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("file.txt"), "data").unwrap();

        let result = resolve_dir_conflict(&c, &source, &target).unwrap();
        assert!(result);
        assert!(!target.exists());
    }

    #[test]
    fn resolve_file_conflict_no_prompt_returns_error() {
        let td = TempDir::new().unwrap();
        let (_, repo) = repo_and_root();
        let c = Context::builder()
            .no_prompt(true)
            .root(td.path().to_path_buf())
            .repository(repo)
            .build();

        let source = td.path().join("source.txt");
        fs::write(&source, "source").unwrap();
        let target = td.path().join("target.txt");
        fs::write(&target, "target").unwrap();

        let result = resolve_file_conflict(&c, &source, &target);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_dir_conflict_no_prompt_returns_error() {
        let td = TempDir::new().unwrap();
        let (_, repo) = repo_and_root();
        let c = Context::builder()
            .no_prompt(true)
            .root(td.path().to_path_buf())
            .repository(repo)
            .build();

        let source = td.path().join("source_dir");
        fs::create_dir_all(&source).unwrap();
        let target = td.path().join("target_dir");
        fs::create_dir_all(&target).unwrap();

        let result = resolve_dir_conflict(&c, &source, &target);
        assert!(result.is_err());
    }

    // ── Display impls fallback (no overlay in ctx) ──────────────────────

    #[test]
    fn ensure_link_display_without_overlay() {
        let (_, repo) = repo_and_root();
        let c = Context::builder()
            .root(PathBuf::from("/tmp"))
            .repository(repo)
            .build();
        let action = EnsureLink::new(
            c,
            PathBuf::from("/overlay/file.txt"),
            PathBuf::from("/target/file.txt"),
        );
        let display = format!("{}", action);
        assert!(display.contains("link:"));
        assert!(display.contains("/overlay/file.txt"));
        assert!(display.contains("/target/file.txt"));
    }

    #[test]
    fn ensure_dir_link_display_without_overlay() {
        let (_, repo) = repo_and_root();
        let c = Context::builder()
            .root(PathBuf::from("/tmp"))
            .repository(repo)
            .build();
        let action = EnsureDirLink::new(
            c,
            PathBuf::from("/overlay/mydir"),
            PathBuf::from("/target/mydir"),
        );
        let display = format!("{}", action);
        assert!(display.contains("link dir:"));
        assert!(display.contains("/overlay/mydir"));
        assert!(display.contains("/target/mydir"));
    }

    #[test]
    fn move_file_display_without_overlay() {
        let (_, repo) = repo_and_root();
        let c = Context::builder()
            .root(PathBuf::from("/tmp"))
            .repository(repo)
            .build();
        let action = MoveFile::new(
            c,
            PathBuf::from("/src/file.txt"),
            PathBuf::from("/dst/file.txt"),
        );
        let display = format!("{}", action);
        assert!(display.contains("move file:"));
        assert!(display.contains("/src/file.txt"));
        assert!(display.contains("/dst/file.txt"));
    }

    // ── Display impls with overlay context ───────────────────────────────

    #[test]
    fn ensure_link_display_with_overlay_context() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_display");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_display").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let source = overlay.root.join("inner/file.txt");
        let target = td.path().join("inner/file.txt");
        let action = EnsureLink::new(c, source, target);
        let display = format!("{}", action);
        assert!(display.contains("link:"));
        assert!(display.contains("inner/file.txt"));
    }

    #[test]
    fn ensure_dir_link_display_with_overlay_context() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_display2");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_display2").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let source = overlay.root.join("mydir");
        let target = td.path().join("mydir");
        let action = EnsureDirLink::new(c, source, target);
        let display = format!("{}", action);
        assert!(display.contains("link dir:"));
        assert!(display.contains("mydir"));
    }

    #[test]
    fn move_file_display_with_overlay_context() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_display3");
        overlay_dir.create_dir_all().unwrap();
        // Escape backslashes so Windows paths (`C:\Users\...`) don't get
        // misparsed as TOML unicode escape sequences (`\U`, `\A`, ...).
        let target_str = td.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target_str))
            .unwrap();
        let overlay = repo.get("ov_display3").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src = td.path().join("myfile.txt");
        let dst = overlay.root.join("myfile.txt");
        let action = MoveFile::new(c, src, dst);
        let display = format!("{}", action);
        assert!(display.contains("move file:"));
        assert!(display.contains("myfile.txt"));
    }

    // ── EnsureDir Display ───────────────────────────────────────────────

    #[test]
    fn ensure_dir_display() {
        let action = EnsureDir::new(PathBuf::from("/some/path/dir"));
        let display = format!("{}", action);
        assert!(display.contains("create directory:"));
        assert!(display.contains("/some/path/dir"));
    }

    // ── EnsureLink already linked (idempotent) ─────────────────────────

    #[tokio::test]
    async fn ensure_link_idempotent_same_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_idem");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_idem").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_file = overlay.root.join("idem.txt");
        fs::write(&src_file, "hello").unwrap();
        let target = td.path().join("idem.txt");

        // Create first
        let action = EnsureLink::new(c.clone(), src_file.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());

        // Should be idempotent
        let action2 = EnsureLink::new(c.clone(), src_file.clone(), target.clone());
        action2.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), src_file);
    }

    // ── EnsureLink dry_run doesn't create symlink ──────────────────────

    #[tokio::test]
    async fn ensure_link_dry_run_no_op() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dry");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dry").unwrap();
        let c = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let src_file = overlay.root.join("dry.txt");
        fs::write(&src_file, "hello").unwrap();
        let target = td.path().join("dry.txt");

        let action = EnsureLink::new(c.clone(), src_file, target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(!target.exists(), "dry_run should not create symlink");
    }

    // ── EnsureDirLink dry_run doesn't create symlink ───────────────────

    #[tokio::test]
    async fn ensure_dir_link_dry_run_no_op() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dry2");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dry2").unwrap();
        let c = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let src_dir = overlay.root.join("dry_dir");
        fs::create_dir_all(&src_dir).unwrap();
        let target = td.path().join("dry_dir");

        let action = EnsureDirLink::new(c.clone(), src_dir, target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(!target.exists(), "dry_run should not create dir symlink");
    }

    // ── MoveFile dry_run doesn't move ──────────────────────────────────

    #[tokio::test]
    async fn move_file_dry_run_no_op() {
        let (td, repo) = repo_and_root();
        let c = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .build();

        let src = td.path().join("dry_src.txt");
        fs::write(&src, "content").unwrap();
        let dst = td.path().join("dry_dst.txt");

        let action = MoveFile::new(c.clone(), src.clone(), dst.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(src.exists(), "source should still exist in dry_run");
        assert!(
            !dst.exists(),
            "destination should not be created in dry_run"
        );
    }

    // ── EnsureLink conflict with force ──────────────────────────────────

    #[tokio::test]
    async fn ensure_link_conflict_with_force_replaces() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_conflict");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_conflict").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_file = overlay.root.join("conflict.txt");
        fs::write(&src_file, "overlay").unwrap();
        let target = td.path().join("conflict.txt");
        fs::write(&target, "existing").unwrap();

        let action = EnsureLink::new(c.clone(), src_file.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), src_file);
    }

    // ── EnsureDirLink conflict with force ───────────────────────────────

    #[tokio::test]
    async fn ensure_dir_link_conflict_with_force_replaces() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dir_conflict");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dir_conflict").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = overlay.root.join("conflict_dir");
        fs::create_dir_all(&src_dir).unwrap();
        let target = td.path().join("conflict_dir");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("existing.txt"), "data").unwrap();

        let action = EnsureDirLink::new(c.clone(), src_dir.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), src_dir);
    }

    // ── EnsureDirLink idempotent ────────────────────────────────────────

    #[tokio::test]
    async fn ensure_dir_link_idempotent_same_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_didem");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_didem").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = overlay.root.join("idem_dir");
        fs::create_dir_all(&src_dir).unwrap();
        let target = td.path().join("idem_dir");

        let action = EnsureDirLink::new(c.clone(), src_dir.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());

        // Second call should be idempotent
        let action2 = EnsureDirLink::new(c.clone(), src_dir.clone(), target.clone());
        action2.execute(c.clone()).await.unwrap();
        assert!(target.is_symlink());
        assert_eq!(fs::read_link(&target).unwrap(), src_dir);
    }

    #[tokio::test]
    async fn ensure_dir_link_creates_dir_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dirlink");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dirlink").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create source directory inside overlay
        let src_dir = overlay.root.join("linked_dir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("file.txt"), "hello").unwrap();

        let target = td.path().join("linked_dir");
        let action = EnsureDirLink::new(c.clone(), src_dir.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();

        assert!(target.is_symlink(), "target should be a symlink");
        assert_eq!(fs::read_link(&target).unwrap(), src_dir);
        assert!(
            target.join("file.txt").exists(),
            "should be able to access files through symlink"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn add_dir_skips_unreadable_entries_with_warning() {
        use std::os::unix::fs::PermissionsExt;

        init_test_tracing();
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_unreadable");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", td.path().display()))
            .unwrap();
        let overlay = repo.get("ov_add_unreadable").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a directory with a readable file and an unreadable subdirectory
        let dir = td.path().join("adddir_unreadable");
        fs::create_dir_all(dir.join("bad_sub")).unwrap();
        fs::write(dir.join("good.txt"), "ok").unwrap();
        fs::write(dir.join("bad_sub").join("hidden.txt"), "secret").unwrap();
        fs::set_permissions(dir.join("bad_sub"), fs::Permissions::from_mode(0o000)).unwrap();

        // add_dir should succeed, skipping the unreadable entry with a warning
        let result = add_dir(c.clone(), &overlay, &dir).await;

        // Restore permissions for cleanup
        fs::set_permissions(dir.join("bad_sub"), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_ok(),
            "add_dir should succeed despite unreadable entries: {:?}",
            result.err()
        );
        // The readable file should have been processed
        assert!(
            overlay.root.join("adddir_unreadable/good.txt").exists(),
            "good.txt should be in overlay"
        );
    }
}
