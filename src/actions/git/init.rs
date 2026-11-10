//! Guarded bootstrap operations, independent of overlay application.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use git2::{FetchOptions, Repository, RepositoryOpenFlags, build::RepoBuilder};

use crate::overlays;

#[derive(Debug)]
pub struct Initialization {
    // Origin rechecks and CLI hook loading must share already-emitted descriptor warnings.
    pub(crate) repository: overlays::Repository,
    pub destination: PathBuf,
    pub cloned: bool,
    pub canonical: Option<String>,
    pub origin: Option<String>,
    pub push_url: Option<String>,
}

impl Initialization {
    pub fn needs_origin_update(&self) -> bool {
        self.canonical.is_some() && self.canonical != self.origin
    }

    /// Recheck the observed URL after confirmation to preserve concurrent edits.
    pub fn set_origin(&mut self) -> Result<()> {
        let Some(canonical) = &self.canonical else {
            return Ok(());
        };
        let repo = open_checkout(&self.destination)?;
        let (origin, push_url) = origin_urls(&repo)?;
        if origin != self.origin || push_url != self.push_url {
            bail!(
                "origin changed during initialization; retry to review its current configuration"
            );
        }
        // Do not overwrite a new canonical value edited while the prompt was open.
        if self.repository.validate_initialization()?.as_ref() != Some(canonical) {
            bail!("repository configuration changed during initialization; retry");
        }
        if origin.is_some() {
            repo.remote_set_url("origin", canonical)
        } else {
            repo.remote("origin", canonical).map(|_| ())
        }
        .with_context(|| {
            format!(
                "failed to set origin in {}; repair Git configuration and retry with --set-origin",
                self.destination.display()
            )
        })?;
        self.origin = Some(canonical.clone());
        Ok(())
    }
}

fn open_checkout(destination: &Path) -> Result<Repository> {
    // Never search parents: an unrelated enclosing repository must not be mutated.
    let repo = Repository::open_ext(
        destination,
        RepositoryOpenFlags::NO_SEARCH,
        &[] as &[&std::ffi::OsStr],
    )
    .with_context(|| {
        format!(
            "{} is not a Git checkout; repair it or choose another --home",
            destination.display()
        )
    })?;
    if repo.is_bare()
        || repo.workdir().map(fs::canonicalize).transpose()? != Some(fs::canonicalize(destination)?)
    {
        bail!(
            "{} must be the root of a non-bare Git checkout",
            destination.display()
        );
    }
    Ok(repo)
}

fn origin_urls(repo: &Repository) -> Result<(Option<String>, Option<String>)> {
    match repo.find_remote("origin") {
        Ok(remote) => Ok((
            Some(remote.url()?.to_owned()),
            remote.pushurl()?.map(str::to_owned),
        )),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok((None, None)),
        Err(error) => Err(error).context("failed to read origin"),
    }
}

/// Clone into an empty destination, or inspect an existing checkout without fetching.
pub fn initialize(destination: PathBuf, source: Option<&str>) -> Result<Initialization> {
    let empty = match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "refusing destination {}: expected an ordinary directory",
                    destination.display()
                );
            }
            fs::read_dir(&destination)?.next().transpose()?.is_none()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).context("failed to inspect initialization destination"),
    };
    let cloned = empty && source.is_some();
    if empty && let Some(source) = source {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut fetch = FetchOptions::new();
        fetch.remote_callbacks(super::remote_callbacks()?);
        RepoBuilder::new()
            .fetch_options(fetch)
            .clone(source, &destination)
            .with_context(|| format!("failed to clone into {}; any partial checkout is preserved: repair it or remove it explicitly before retrying", destination.display()))?;
    } else if empty {
        bail!(
            "no checkout at {}; supply a Git source: over init <SOURCE>",
            destination.display()
        );
    }
    finish_initialization(destination, cloned)
}

fn finish_initialization(destination: PathBuf, cloned: bool) -> Result<Initialization> {
    let repo = open_checkout(&destination)?;
    let repository = overlays::Repository::new(destination.clone());
    let canonical = repository.validate_initialization().with_context(|| {
        format!(
            "invalid Over repository at {}; checkout preserved: fix its root descriptor and retry",
            destination.display()
        )
    })?;
    let (origin, push_url) = origin_urls(&repo)?;
    let mut result = Initialization {
        repository,
        destination,
        cloned,
        canonical,
        origin,
        push_url,
    };
    if cloned && result.needs_origin_update() {
        result.set_origin()?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_https_origin_becomes_ssh_without_contacting_either_remote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(tmp.path()).unwrap();
        repo.remote("origin", "https://example.invalid/user/dotfiles.git")
            .unwrap();
        fs::write(
            tmp.path().join("over.toml"),
            "repository = 'git@example.invalid:user/dotfiles.git'",
        )
        .unwrap();
        // Exercise post-clone finalization without requiring a public HTTPS server.
        let result = finish_initialization(tmp.path().to_path_buf(), true).unwrap();
        assert_eq!(
            result.origin.as_deref(),
            Some("git@example.invalid:user/dotfiles.git")
        );
        assert!(!result.needs_origin_update());
    }
}
