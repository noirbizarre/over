//! Guarded bootstrap operations, independent of overlay application.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use git2::{
    FetchOptions, Repository, RepositoryOpenFlags,
    build::{CheckoutBuilder, RepoBuilder},
};

use crate::overlays::{self, MaterializationKind};

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
    /// Create repository policy only when no supported root descriptor exists.
    pub fn create_configuration(&self, mode: MaterializationKind) -> Result<PathBuf> {
        let mode = match mode {
            MaterializationKind::Checkout => "checkout",
            MaterializationKind::Symlink => "symlink",
            _ => bail!("repository setup supports checkout or symlink mode"),
        };
        // Confirmation can take arbitrarily long; do not overwrite a descriptor added meanwhile.
        if self.repository.root_descriptor()?.is_some() {
            bail!("root configuration appeared during initialization; retry to review it");
        }
        let path = self.destination.join("over.toml");
        // Publish only complete content, so a write failure cannot leave an empty default policy.
        let mut file = tempfile::NamedTempFile::new_in(&self.destination)
            .context("failed to prepare root configuration")?;
        file.write_all(format!("mode = \"{mode}\"\n").as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        file.persist_noclobber(&path)
            .with_context(|| format!("failed to create {} without overwriting", path.display()))?;
        self.repository.validate_initialization()?;
        Ok(path)
    }

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

fn head_state(repo: &Repository) -> Result<(Option<String>, Option<git2::Oid>)> {
    let head = repo.find_reference("HEAD")?;
    let target = match repo.head() {
        Ok(head) => head.target(),
        Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
        Err(error) => return Err(error.into()),
    };
    Ok((head.symbolic_target()?.map(str::to_owned), target))
}

fn index_state(repo: &Repository) -> Result<Option<Vec<u8>>> {
    match fs::read(repo.path().join("index")) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to inspect pending initial index"),
    }
}

fn configure_checkout(repo: &Repository) -> Result<crate::crypt::detect::Detection> {
    let crypt = crate::crypt::detect::inspect(repo)
        .context("pre-checkout crypt detection failed; clone preserved for repair")?;
    crypt
        .validate_checkout()
        .context("initial checkout was not started; clone preserved for repair")?;
    if crypt.is_crypt() {
        // Locked smudge retains ciphertext, but the trusted driver must exist before checkout.
        crate::crypt::access::configure_filters(repo)
            .context("driver setup failed before initial checkout; clone preserved for repair")?;
    }
    Ok(crypt)
}

/// Clone into an empty destination, or inspect an existing checkout without fetching.
pub fn initialize(destination: PathBuf, source: Option<&str>) -> Result<Initialization> {
    initialize_checkout(destination, source, false)
}

/// Prepare initialization while allowing the caller to offer missing root configuration.
pub fn prepare(destination: PathBuf, source: Option<&str>) -> Result<Initialization> {
    initialize_checkout(destination, source, true)
}

/// Offer credential acquisition only after committed validation and local driver setup.
pub(crate) fn prepare_with(
    destination: PathBuf,
    source: Option<&str>,
    before_checkout: impl FnOnce(&Repository, &crate::crypt::detect::Detection) -> Result<()>,
) -> Result<Initialization> {
    initialize_with(destination, source, true, before_checkout)
}

fn initialize_checkout(
    destination: PathBuf,
    source: Option<&str>,
    allow_missing: bool,
) -> Result<Initialization> {
    initialize_with(destination, source, allow_missing, |_, _| Ok(()))
}

fn initialize_with(
    destination: PathBuf,
    source: Option<&str>,
    allow_missing: bool,
    before_checkout: impl FnOnce(&Repository, &crate::crypt::detect::Detection) -> Result<()>,
) -> Result<Initialization> {
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
        let mut checkout = CheckoutBuilder::new();
        checkout.dry_run();
        let repo = RepoBuilder::new()
            .fetch_options(fetch)
            .with_checkout(checkout)
            .clone(source, &destination)
            .with_context(|| format!("failed to clone into {}; any partial checkout is preserved: repair it or remove it explicitly before retrying", destination.display()))?;
        let head = head_state(&repo)?;
        let crypt = configure_checkout(&repo)?;
        let index = index_state(&repo)?;
        before_checkout(&repo, &crypt)?;
        // Consent may take arbitrarily long; never reset files or refs changed meanwhile.
        ensure!(
            head_state(&repo)? == head
                && index_state(&repo)? == index
                && fs::read_dir(&destination)?
                    .all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".git")),
            "clone changed during initialization; initial checkout not started; inspect {} before retrying in a new destination",
            destination.display()
        );
        if let Some(tree) = crypt.tree {
            // An empty clone has no HEAD to reset; root configuration setup handles it below.
            let mut command = crate::crypt::policy::git_command(&repo)?;
            command.args(["reset", "--hard", "HEAD"]);
            if crypt.is_crypt() {
                command.env(
                    crate::crypt::bootstrap::ENV,
                    crate::crypt::bootstrap::Context::new(&repo, tree)?.encode()?,
                );
            }
            let output = command.output()?;
            ensure!(
                output.status.success(),
                "initial filtered checkout failed; partial checkout preserved at {}: {}; inspect it before choosing a new destination or explicitly removing the failed clone",
                destination.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            if crypt.is_crypt() && !crate::crypt::access::local_key_missing(&repo)? {
                crate::crypt::access::local_key(&repo)?;
            }
        }
    } else if empty {
        bail!(
            "no checkout at {}; supply a Git source: over init <SOURCE>",
            destination.display()
        );
    }
    finish_initialization(destination, cloned, allow_missing)
}

fn finish_initialization(
    destination: PathBuf,
    cloned: bool,
    allow_missing: bool,
) -> Result<Initialization> {
    let repo = open_checkout(&destination)?;
    let repository = overlays::Repository::new(destination.clone());
    let validate = || {
        if allow_missing && repository.root_descriptor()?.is_none() {
            return Ok(None);
        }
        repository.validate_initialization()
    };
    let canonical = validate().with_context(|| {
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

    #[rstest::rstest]
    #[case("worktree")]
    #[case("index")]
    #[case("branch")]
    fn changes_made_while_initialization_waits_abort_checkout_without_overwriting_them(
        #[case] change: &str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let source = Repository::init(directory.path().join("source")).unwrap();
        let mut builder = source.treebuilder(None).unwrap();
        builder
            .insert(
                "over.toml",
                source.blob(b"target = '~'\n").unwrap(),
                0o100644,
            )
            .unwrap();
        let tree = source.find_tree(builder.write().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        source
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        let destination = directory.path().join("clone");
        let error = prepare_with(
            destination.clone(),
            source.workdir().unwrap().to_str(),
            |repo, _| {
                assert!(!destination.join("over.toml").exists());
                match change {
                    "worktree" => fs::write(destination.join("independent"), b"preserve me")?,
                    "index" => fs::write(repo.path().join("index"), b"independent index")?,
                    "branch" => {
                        repo.branch("independent", &repo.head()?.peel_to_commit()?, false)?;
                        repo.set_head("refs/heads/independent")?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("clone changed during initialization")
        );
        assert!(!destination.join("over.toml").exists());
        match change {
            "worktree" => assert_eq!(
                fs::read(destination.join("independent")).unwrap(),
                b"preserve me"
            ),
            "index" => assert_eq!(
                fs::read(destination.join(".git/index")).unwrap(),
                b"independent index"
            ),
            "branch" => assert_eq!(
                Repository::open(&destination)
                    .unwrap()
                    .head()
                    .unwrap()
                    .name()
                    .unwrap(),
                "refs/heads/independent"
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn failed_driver_configuration_preserves_a_clone_without_materializing_tracked_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = Repository::init(directory.path().join("source")).unwrap();
        let identity = age::x25519::Identity::generate();
        crate::crypt::access::add_key(
            &source,
            "alice",
            "default",
            Some(&identity.to_public().to_string()),
            None,
            true,
        )
        .unwrap();
        let mut index = source.index().unwrap();
        index
            .add_all(["."], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree = source.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        source
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "metadata only",
                &tree,
                &[],
            )
            .unwrap();
        let destination = directory.path().join("clone");
        let mut checkout = CheckoutBuilder::new();
        checkout.dry_run();
        let clone = RepoBuilder::new()
            .with_checkout(checkout)
            .clone(source.workdir().unwrap().to_str().unwrap(), &destination)
            .unwrap();
        let config = fs::read(clone.path().join("config")).unwrap();
        fs::write(clone.path().join("config.lock"), b"another writer").unwrap();
        let error = configure_checkout(&clone).unwrap_err();
        assert!(format!("{error:#}").contains("driver setup failed before initial checkout"));
        assert_eq!(fs::read(clone.path().join("config")).unwrap(), config);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        assert!(!clone.path().join("over-crypt/key").exists());
        fs::remove_file(clone.path().join("config.lock")).unwrap();
        assert!(configure_checkout(&clone).unwrap().is_crypt());
        assert!(
            clone
                .config()
                .unwrap()
                .get_bool("filter.over.required")
                .unwrap()
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    }

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
        let result = finish_initialization(tmp.path().to_path_buf(), true, false).unwrap();
        assert_eq!(
            result.origin.as_deref(),
            Some("git@example.invalid:user/dotfiles.git")
        );
        assert!(!result.needs_origin_update());
    }
}
