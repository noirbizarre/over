//! Deletion authority is stricter than reporting a checkout as "clean".

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use dialoguer::Confirm;
use git2::{Repository, RepositoryState, StatusOptions};

use crate::desired::{DesiredEntry, Provenance};
use crate::exec::{Context, Ctx};

/// A hard boundary failure cannot be turned into consent by any flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Assessment {
    RequiresConsent {
        risks: Vec<String>,
        identity: String,
    },
    Forbidden(String),
}

impl Assessment {
    pub(crate) fn description(&self) -> String {
        match self {
            Self::RequiresConsent { risks, .. } => risks.join("; "),
            Self::Forbidden(reason) => reason.clone(),
        }
    }
}

/// Reject symlinks inside an owned unit while allowing established aliases above it.
pub(crate) fn validate_relative_ancestors(root: &Path, path: &Path) -> Result<()> {
    for ancestor in path
        .ancestors()
        .skip(1)
        .take_while(|ancestor| ancestor.starts_with(root))
    {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "refusing to follow symlink ancestor '{}' of '{}'",
                ancestor.display(),
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Snapshot physical parent identity without dereferencing the selected leaf.
/// Existing ancestor aliases are supported; a later retargeting is not.
#[derive(Debug, Clone)]
pub(crate) struct PathGuard {
    path: std::path::PathBuf,
    location: std::path::PathBuf,
    directory_identity: Option<String>,
}

impl PathGuard {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.into(),
            location: crate::overlays::mapping::location(path)?,
            directory_identity: directory_identity(path)?,
        })
    }

    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            crate::overlays::mapping::location(&self.path)? == self.location,
            "ancestor of '{}' changed since planning; rerun to re-evaluate",
            self.path.display()
        );
        if let Some(identity) = &self.directory_identity {
            if fs::symlink_metadata(&self.path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                // A prior owned removal may already have removed this scaffold.
                return Ok(());
            }
            ensure!(
                directory_identity(&self.path)?.as_ref() == Some(identity),
                "directory '{}' was replaced since planning",
                self.path.display()
            );
        }
        Ok(())
    }
}

fn directory_identity(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(format!("{}:{}", metadata.dev(), metadata.ino())))
    }
    #[cfg(not(unix))]
    {
        Ok(Some(format!("{:?}", metadata.created()?)))
    }
}

pub(crate) fn assess(entry: &DesiredEntry) -> Result<Assessment> {
    let forbidden = |reason: &str| Ok(Assessment::Forbidden(reason.into()));
    if fs::symlink_metadata(&entry.target)?
        .file_type()
        .is_symlink()
    {
        return forbidden("checkout target is a symlink, not an owned deletion unit");
    }
    let repo = match Repository::open(&entry.target) {
        Ok(repo) => repo,
        Err(_) => return forbidden("target is not a recognized real checkout"),
    };
    // External gitdirs and linked worktrees can own data outside this target.
    let expected_git = entry.target.join(".git");
    if !expected_git.is_dir()
        || expected_git.is_symlink()
        || repo.path().canonicalize()? != expected_git.canonicalize()?
        || !repo.worktrees()?.is_empty()
    {
        return forbidden("shared, external, or bare Git layout requires manual removal");
    }
    let associated = repo.config()?.get_string("over.overlay").ok();
    match &entry.provenance {
        Provenance::Git {
            overlay, config, ..
        } => {
            if associated.as_deref().is_some_and(|name| name != overlay)
                || repo
                    .find_remote("origin")
                    .ok()
                    .and_then(|remote| remote.url().ok().map(String::from))
                    .as_deref()
                    != Some(config.url.as_str())
            {
                return forbidden(
                    "checkout identity does not match its declared overlay/repository",
                );
            }
            if config.worktree || config.worktrees.is_some() {
                return forbidden("worktree containers require manual removal");
            }
        }
        Provenance::Overlay { overlay, source } => {
            if associated.as_deref() != Some(overlay.as_str()) {
                return forbidden(
                    "old checkout has no matching overlay association; restore its old declaration or move it aside",
                );
            }
            let target = entry.target.canonicalize()?;
            let source = source.canonicalize()?;
            if source.starts_with(&target) || target.starts_with(&source) {
                return forbidden("checkout deletion overlaps the overlay source");
            }
        }
        Provenance::SymlinkSidecar {
            overlay, config, ..
        } => {
            if associated.as_deref() != Some(overlay.as_str()) {
                return forbidden("checkout does not belong to this sidecar's overlay");
            }
            if config
                .canonicalize()?
                .starts_with(entry.target.canonicalize()?)
            {
                return forbidden("checkout deletion contains overlay configuration");
            }
        }
        _ => return forbidden("declaration does not establish checkout ownership"),
    }
    check_nested_units(&entry.target)?;
    let mut risks = Vec::new();
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true);
    if !repo.statuses(Some(&mut options))?.is_empty() {
        risks.push("tracked, staged, untracked, or ignored files may be lost".into());
    }
    if repo.state() != RepositoryState::Clean {
        risks.push("an unfinished Git operation will be lost".into());
    }
    // Even equal upstream tips do not back up stashes, other refs, reflogs,
    // hooks or local configuration. Do not promise recoverability from status.
    risks.push("all local Git history, refs, reflogs, hooks and configuration will be removed; external recovery is not guaranteed".into());
    let identity = format!(
        "{:?}:{:?}:{:?}",
        directory_identity(&entry.target)?,
        repo.head().ok().and_then(|head| head.target()),
        repo.references()?
            .map(|reference| reference
                .map(|reference| (reference.name_bytes().to_vec(), reference.target())))
            .collect::<std::result::Result<Vec<_>, _>>()?
    );
    Ok(Assessment::RequiresConsent { risks, identity })
}

fn check_nested_units(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name() == ".git" || !entry.file_type()?.is_dir() {
            continue;
        }
        let child = entry.path();
        ensure!(
            !child.join(".git").exists()
                && super::virtual_checkout::state::record_for_blocking(&child)?.is_none(),
            "checkout contains an independent repository/checkout at '{}'",
            child.display()
        );
        check_nested_units(&child)?;
    }
    Ok(())
}

pub(crate) fn authorize(ctx: &Context, target: &Path, assessment: &Assessment) -> Result<()> {
    if !ctx.repository.root.as_os_str().is_empty() && ctx.repository.root.exists() {
        ensure!(
            !ctx.repository
                .root
                .canonicalize()?
                .starts_with(target.canonicalize()?),
            "checkout deletion contains the overlay source repository"
        );
    }
    authorize_with(ctx, target, assessment, |prompt| {
        Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()
            .context("checkout disposal confirmation failed")
    })
}

pub(crate) enum Removal {
    Removed,
    Retained(String),
}

/// Keep recursive inspection, terminal interaction and removal off the async
/// executor. A refusal is distinct from an I/O error after removal starts.
pub(crate) async fn remove_checkout(ctx: Ctx, entry: DesiredEntry) -> Result<Removal> {
    tokio::task::spawn_blocking(move || {
        let guard = PathGuard::capture(&entry.target)?;
        let assessment = assess(&entry)?;
        if let Err(error) = authorize(&ctx, &entry.target, &assessment) {
            return Ok(Removal::Retained(error.to_string()));
        }
        guard.verify()?;
        ensure!(
            assess(&entry)? == assessment,
            "checkout changed during disposal confirmation"
        );
        fs::remove_dir_all(&entry.target).with_context(|| {
            format!(
                "checkout removal failed at '{}'; removal may be partial",
                entry.target.display()
            )
        })?;
        Ok(Removal::Removed)
    })
    .await?
}

fn authorize_with(
    ctx: &Context,
    target: &Path,
    assessment: &Assessment,
    confirm: impl FnOnce(String) -> Result<bool>,
) -> Result<()> {
    match assessment {
        Assessment::Forbidden(reason) => {
            bail!("refusing to remove '{}': {reason}", target.display())
        }
        Assessment::RequiresConsent { risks, .. } => {
            if ctx.discard_checkout {
                return Ok(());
            }
            ensure!(
                !ctx.no_prompt && !ctx.force,
                "checkout disposal at '{}' requires --discard-checkout or interactive confirmation: {}",
                target.display(),
                risks.join("; ")
            );
            ensure!(
                confirm(format!(
                    "Permanently remove '{}'? {}",
                    target.display(),
                    risks.join("; ")
                ))?,
                "checkout disposal declined at '{}'",
                target.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(false, false, false, true, true)]
    #[case(false, false, false, false, false)]
    #[case(true, false, false, true, false)]
    #[case(false, true, false, true, false)]
    #[case(true, true, true, false, true)]
    fn checkout_loss_requires_its_own_explicit_choice(
        #[case] no_prompt: bool,
        #[case] force: bool,
        #[case] discard: bool,
        #[case] answer: bool,
        #[case] allowed: bool,
    ) {
        let ctx = Context::builder()
            .no_prompt(no_prompt)
            .force(force)
            .discard_checkout(discard)
            .build();
        let assessment = Assessment::RequiresConsent {
            risks: vec!["local history".into()],
            identity: "unit".into(),
        };
        let result = authorize_with(&ctx, Path::new("checkout"), &assessment, |_| {
            assert!(!no_prompt && !force && !discard);
            Ok(answer)
        });
        assert_eq!(result.is_ok(), allowed);
        assert!(
            authorize_with(
                &ctx,
                Path::new("checkout"),
                &Assessment::Forbidden("foreign".into()),
                |_| panic!("must not prompt")
            )
            .is_err()
        );
    }
}
