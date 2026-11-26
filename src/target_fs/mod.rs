//! Invocation-scoped privileged effects. Configuration and content preparation stay in the caller.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod unix;

/// Wire paths preserve Unix filenames which are not UTF-8.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct WirePath(Vec<u8>);

impl WirePath {
    fn new(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(not(unix))]
        Self(path.to_string_lossy().as_bytes().to_vec())
    }

    fn path(&self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(self.0.clone()).into()
        }
        #[cfg(not(unix))]
        PathBuf::from(String::from_utf8_lossy(&self.0).as_ref())
    }
}

// Never derive Debug: prepared hook scripts and write buffers can contain secrets.
#[derive(Serialize, Deserialize)]
pub(crate) enum Request {
    Prepare {
        id: u64,
        path: WirePath,
        contents: Vec<(WirePath, PreparedNode)>,
    },
    PrepareFile {
        id: u64,
        path: WirePath,
        update: Option<(Vec<u8>, u32)>,
    },
    Apply {
        id: u64,
    },
    Rollback {
        id: u64,
    },
    Finish {
        id: u64,
    },
    Read {
        path: WirePath,
    },
    Inspect {
        path: WirePath,
        follow: bool,
    },
    ReadLink {
        path: WirePath,
    },
    ReadDir {
        path: WirePath,
    },
    Location {
        path: WirePath,
    },
    Physical {
        path: WirePath,
    },
    CreateDir {
        path: WirePath,
    },
    Symlink {
        source: WirePath,
        target: WirePath,
    },
    HardLink {
        source: WirePath,
        target: WirePath,
    },
    RemoveFile {
        path: WirePath,
    },
    RemoveDir {
        path: WirePath,
    },
    Write {
        path: WirePath,
        bytes: Vec<u8>,
        mode: Option<u32>,
    },
    SecureWrite {
        path: WirePath,
        bytes: Vec<u8>,
        before: Option<Vec<u8>>,
        mode: u32,
    },
    Metadata {
        path: WirePath,
        mode: Option<u32>,
        owner: Option<u32>,
        group: Option<u32>,
        expected: Option<MetadataIdentity>,
    },
    Hook {
        script: String,
        cwd: WirePath,
        env: Vec<(WirePath, WirePath)>,
        remove_env: Vec<WirePath>,
    },
}

/// A no-follow identity, carried into the helper rather than recaptured at mutation time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MetadataIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    links: u64,
    #[cfg(not(unix))]
    created: std::time::SystemTime,
}

impl MetadataIdentity {
    pub(crate) fn capture(metadata: &std::fs::Metadata) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Creating managed child directories legitimately changes a directory's link count.
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                links: if metadata.is_dir() {
                    0
                } else {
                    metadata.nlink()
                },
            })
        }
        #[cfg(not(unix))]
        Ok(Self {
            created: metadata.created()?,
        })
    }

    pub(crate) fn verify(&self, metadata: &std::fs::Metadata) -> Result<()> {
        anyhow::ensure!(
            *self == Self::capture(metadata)?,
            "target changed since metadata inspection; rerun to re-evaluate"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MetadataGuard {
    fs: TargetFs,
    path: PathBuf,
    identity: MetadataIdentity,
}

impl MetadataGuard {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn capture(fs: &TargetFs, path: &Path) -> Result<Self> {
        Ok(Self {
            fs: fs.clone(),
            path: path.into(),
            identity: MetadataIdentity::capture(&fs.metadata(path, false)?)?,
        })
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.identity.verify(&self.fs.metadata(&self.path, false)?)
    }

    pub(crate) fn apply(&self, permissions: &crate::overlays::Permissions) -> Result<()> {
        self.fs
            .set_metadata_with_identity(&self.path, permissions, Some(self.identity.clone()))
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) enum PreparedNode {
    Directory,
    File(Vec<u8>, i32),
}

/// The parent owns consent and sequencing; the helper retains filesystem-only backups.
pub(crate) struct PreparedEffect {
    fs: TargetFs,
    id: u64,
    applied: bool,
}

impl PreparedEffect {
    pub(crate) fn apply(&mut self) -> Result<()> {
        self.fs.effect(Request::Apply { id: self.id })?;
        self.applied = true;
        Ok(())
    }

    pub(crate) fn rollback(&mut self) -> Result<()> {
        self.fs.effect(Request::Rollback { id: self.id })?;
        self.applied = false;
        Ok(())
    }
}

impl Drop for PreparedEffect {
    fn drop(&mut self) {
        if !self.applied
            && let Err(error) = self.rollback()
        {
            tracing::warn!("could not roll back privileged preparation: {error}");
        }
        if let Err(error) = self.fs.effect(Request::Finish { id: self.id }) {
            tracing::warn!("could not finalize privileged filesystem backups: {error}");
        }
    }
}

impl Request {
    #[cfg(unix)]
    fn accepts(&self, reply: &Reply) -> bool {
        match (self, reply) {
            (_, Reply::Error { .. }) => true,
            (Self::Read { .. }, Reply::Bytes(_))
            | (Self::Inspect { .. }, Reply::Descriptor)
            | (Self::ReadDir { .. }, Reply::Paths(_))
            | (Self::Hook { .. }, Reply::Exit(_)) => true,
            (
                Self::ReadLink { .. } | Self::Location { .. } | Self::Physical { .. },
                Reply::Paths(paths),
            ) => paths.len() == 1,
            (_, Reply::Done) => self.mutates() && !matches!(self, Self::Hook { .. }),
            _ => false,
        }
    }
    fn mutates(&self) -> bool {
        !matches!(
            self,
            Self::Read { .. }
                | Self::Inspect { .. }
                | Self::ReadLink { .. }
                | Self::ReadDir { .. }
                | Self::Location { .. }
                | Self::Physical { .. }
        )
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) enum Reply {
    Done,
    Bytes(Vec<u8>),
    Paths(Vec<WirePath>),
    Descriptor,
    Exit(i32),
    Error {
        kind: Option<String>,
        message: String,
    },
}

/// One session can be injected into every context in a library invocation.
#[derive(Default)]
pub struct Session {
    identities: Mutex<std::collections::HashMap<(crate::overlays::Identity, bool), u32>>,
    #[cfg(unix)]
    state: Mutex<unix::State>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn identity(
        &self,
        identity: &crate::overlays::Identity,
        group: bool,
    ) -> Result<u32> {
        let key = (identity.clone(), group);
        let mut identities = self
            .identities
            .lock()
            .map_err(|_| anyhow::anyhow!("ownership identity cache lock poisoned"))?;
        if let Some(id) = identities.get(&key) {
            return Ok(*id);
        }
        let id = resolve_identity(identity, group)?;
        identities.insert(key, id);
        Ok(id)
    }
    pub fn shutdown(&self) -> Result<()> {
        #[cfg(unix)]
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("privileged session lock poisoned during shutdown"))?;
            state.shutdown()?;
            *state = unix::State::Failed;
        }
        Ok(())
    }
    /// CLI contexts created independently must still reuse the invocation's authentication.
    pub fn shared() -> Arc<Self> {
        static SESSION: OnceLock<Arc<Session>> = OnceLock::new();
        SESSION.get_or_init(|| Arc::new(Self::default())).clone()
    }

    pub fn target(self: &Arc<Self>, sudo: bool, no_prompt: bool, read_only: bool) -> TargetFs {
        TargetFs {
            session: self.clone(),
            sudo,
            no_prompt,
            read_only,
        }
    }

    #[cfg(unix)]
    fn request(&self, request: Request, no_prompt: bool, read_only: bool) -> Result<Reply> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("privileged filesystem session lock poisoned"))?
            .request(request, no_prompt, read_only)
    }

    #[cfg(not(unix))]
    fn request(&self, _request: Request, _no_prompt: bool, _read_only: bool) -> Result<Reply> {
        bail!("sudo mode is only supported on Unix")
    }
}

/// Privilege is selected explicitly, never by retrying a failed local operation.
#[derive(Clone, Debug)]
pub struct TargetFs {
    session: Arc<Session>,
    sudo: bool,
    no_prompt: bool,
    read_only: bool,
}

#[derive(Clone, Debug)]
pub struct Access {
    pub fs: TargetFs,
    pub permissions: crate::overlays::Permissions,
    overlay: Option<Arc<crate::overlays::Overlay>>,
    logical: PathBuf,
}

impl Default for Access {
    fn default() -> Self {
        Self {
            fs: Session::shared().target(false, false, false),
            permissions: Default::default(),
            overlay: None,
            logical: PathBuf::new(),
        }
    }
}

impl Access {
    pub(crate) fn inspection(overlay: &crate::overlays::Overlay, logical: &Path) -> Self {
        let properties = crate::overlays::rules::resolve_with_origins(overlay, logical, false);
        Self {
            fs: Session::shared().target(properties.sudo, false, true),
            permissions: properties.metadata,
            overlay: Some(Arc::new(overlay.clone())),
            logical: logical.to_owned(),
        }
    }
    pub(crate) fn new(
        ctx: &crate::exec::Context,
        overlay: Arc<crate::overlays::Overlay>,
        logical: &Path,
        directory: bool,
    ) -> Self {
        let properties = crate::overlays::rules::resolve_with_origins(&overlay, logical, directory);
        Self {
            fs: ctx
                .target_fs
                .target(properties.sudo, ctx.no_prompt, ctx.dry_run || ctx.read_only),
            permissions: properties.metadata,
            overlay: Some(overlay),
            logical: logical.to_owned(),
        }
    }

    pub fn child(&self, relative: &Path, directory: bool) -> Self {
        let Some(overlay) = &self.overlay else {
            return self.clone();
        };
        let logical = self.logical.join(relative);
        let properties = crate::overlays::rules::resolve_with_origins(overlay, &logical, directory);
        let mut fs = self.fs.clone();
        fs.sudo = properties.sudo;
        Self {
            fs,
            permissions: properties.metadata,
            overlay: self.overlay.clone(),
            logical,
        }
    }

    pub fn has_metadata(&self) -> bool {
        self.permissions.mode.is_some()
            || self.permissions.owner.is_some()
            || self.permissions.group.is_some()
    }

    pub(crate) fn validate_identities(&self) -> Result<()> {
        for (identity, group) in [
            (&self.permissions.owner, false),
            (&self.permissions.group, true),
        ] {
            if let Some(identity) = identity {
                self.fs.session.identity(identity, group)?;
            }
        }
        Ok(())
    }

    pub(crate) fn needs_independent_link_ownership(&self, source: &Path) -> Result<bool> {
        if self.permissions.owner.is_none() && self.permissions.group.is_none() {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(source)?;
            let owner = self
                .permissions
                .owner
                .as_ref()
                .map(|identity| self.fs.session.identity(identity, false))
                .transpose()?;
            let group = self
                .permissions
                .group
                .as_ref()
                .map(|identity| self.fs.session.identity(identity, true))
                .transpose()?;
            Ok(owner.is_some_and(|owner| owner != metadata.uid())
                || group.is_some_and(|group| group != metadata.gid()))
        }
        #[cfg(not(unix))]
        {
            let _ = source;
            bail!("ownership policy requires Unix")
        }
    }

    pub(crate) fn has_metadata_policy(&self) -> bool {
        self.has_metadata()
            || self.overlay.as_ref().is_some_and(|overlay| {
                overlay.scopes.iter().any(|scope| {
                    scope.permissions.is_some()
                        || scope.rules.iter().any(|rule| rule.permissions.is_some())
                })
            })
    }

    pub(crate) fn remove_target(&self, path: &Path) -> Result<()> {
        self.validate_removal(path)?;
        let metadata = self.fs.metadata(path, false)?;
        if metadata.is_dir() {
            match self.fs.metadata(&path.join(".git"), false) {
                Err(error) if is_not_found(&error) => {}
                _ => bail!(
                    "refusing to remove protected Git checkout '{}'",
                    path.display()
                ),
            }
            anyhow::ensure!(
                crate::materialize::virtual_checkout::state::record_for_blocking(path)?.is_none(),
                "refusing to remove protected virtual checkout"
            );
            // Reconcile children individually so an explicit child sudo:false remains authoritative.
            for child in self.fs.read_dir(path)? {
                self.child(child.strip_prefix(path)?, false)
                    .remove_target(&child)?;
            }
            self.fs.remove_dir(path)
        } else {
            self.fs.remove_file(path)
        }
    }

    pub(crate) fn validate_removal(&self, path: &Path) -> Result<()> {
        let metadata = self.fs.metadata(path, false)?;
        if metadata.is_dir() {
            match self.fs.metadata(&path.join(".git"), false) {
                Err(error) if is_not_found(&error) => {}
                _ => bail!(
                    "refusing to replace protected Git checkout '{}'",
                    path.display()
                ),
            }
            anyhow::ensure!(
                crate::materialize::virtual_checkout::state::record_for_blocking(path)?.is_none(),
                "refusing to replace protected virtual checkout"
            );
            for child in self.fs.read_dir(path)? {
                self.child(child.strip_prefix(path)?, false)
                    .validate_removal(&child)?;
            }
        }
        Ok(())
    }

    pub(crate) fn metadata_drift(
        &self,
        path: &Path,
    ) -> Result<Option<(crate::overlays::Permissions, crate::overlays::Permissions)>> {
        if !self.has_metadata() {
            return Ok(None);
        }
        #[cfg(unix)]
        {
            use crate::overlays::{FileMode, Identity, Permissions};
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = self.fs.metadata(path, false)?;
            let desired = Permissions {
                mode: if metadata.is_symlink() {
                    None
                } else {
                    self.permissions.mode
                },
                owner: self
                    .permissions
                    .owner
                    .as_ref()
                    .map(|identity| self.fs.session.identity(identity, false).map(Identity::Id))
                    .transpose()?,
                group: self
                    .permissions
                    .group
                    .as_ref()
                    .map(|identity| self.fs.session.identity(identity, true).map(Identity::Id))
                    .transpose()?,
            };
            let current = Permissions {
                mode: desired
                    .mode
                    .map(|_| FileMode::from_bits(metadata.permissions().mode())),
                owner: desired.owner.as_ref().map(|_| Identity::Id(metadata.uid())),
                group: desired.group.as_ref().map(|_| Identity::Id(metadata.gid())),
            };
            Ok((current != desired).then_some((current, desired)))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            anyhow::ensure!(
                self.permissions.owner.is_none() && self.permissions.group.is_none(),
                "ownership policy is only supported on Unix"
            );
            Ok(None)
        }
    }
}

impl TargetFs {
    fn elevates_reads(&self) -> bool {
        // Inspections and previews must not authenticate, even for sudo-managed targets.
        self.sudo && !self.read_only
    }

    pub fn is_privileged(&self) -> bool {
        self.sudo
    }

    pub(crate) fn prepare_inventory(
        &self,
        path: &Path,
        contents: Vec<(PathBuf, PreparedNode)>,
    ) -> Result<PreparedEffect> {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.effect(Request::Prepare {
            id,
            path: WirePath::new(path),
            contents: contents
                .into_iter()
                .map(|(path, node)| (WirePath::new(&path), node))
                .collect(),
        })?;
        Ok(PreparedEffect {
            fs: self.clone(),
            id,
            applied: false,
        })
    }

    pub(crate) fn prepare_file(
        &self,
        path: &Path,
        update: Option<(Vec<u8>, u32)>,
    ) -> Result<PreparedEffect> {
        static NEXT_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(u64::MAX / 2);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.effect(Request::PrepareFile {
            id,
            path: WirePath::new(path),
            update,
        })?;
        Ok(PreparedEffect {
            fs: self.clone(),
            id,
            applied: false,
        })
    }

    pub fn location(&self, path: &Path) -> Result<PathBuf> {
        if !self.elevates_reads() {
            crate::desired::ownership::validate_directory_ancestors(path)?;
            return crate::overlays::mapping::location(path);
        }
        match self.request(Request::Location {
            path: WirePath::new(path),
        })? {
            Reply::Paths(mut paths) if paths.len() == 1 => Ok(paths.remove(0).path()),
            _ => bail!("unexpected privileged location response"),
        }
    }

    pub(crate) fn physical_destination(&self, target: &Path, source: &Path) -> Result<PathBuf> {
        if !self.elevates_reads() {
            return crate::overlays::mapping::physical_destination(target, source);
        }
        let metadata = match self.metadata(target, false) {
            Ok(metadata) => Some(metadata),
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        if metadata
            .as_ref()
            .is_some_and(|metadata| metadata.is_symlink())
        {
            let referent = self.read_link(target)?;
            let absolute = if referent.is_absolute() {
                referent
            } else {
                target
                    .parent()
                    .context("mapped target has no parent")?
                    .join(referent)
            };
            // A managed dangling link still owns its location after its source was removed.
            if crate::overlays::mapping::absolute(&absolute)?
                == crate::overlays::mapping::absolute(source)?
            {
                return self.location(target);
            }
        }
        let physical = self.physical(target)?;
        if metadata.is_some_and(|metadata| metadata.is_symlink())
            && physical == crate::overlays::mapping::physical(source)?
        {
            self.location(target)
        } else {
            Ok(physical)
        }
    }

    pub(crate) fn physical(&self, path: &Path) -> Result<PathBuf> {
        if !self.elevates_reads() {
            return crate::overlays::mapping::physical(path);
        }
        match self.request(Request::Physical {
            path: WirePath::new(path),
        })? {
            Reply::Paths(mut paths) if paths.len() == 1 => Ok(paths.remove(0).path()),
            _ => bail!("unexpected privileged physical-path response"),
        }
    }

    pub fn read_to_string(&self, path: &Path) -> Result<String> {
        String::from_utf8(self.read(path)?).context("target is not UTF-8")
    }
    fn request(&self, request: Request) -> Result<Reply> {
        anyhow::ensure!(
            !self.read_only || !request.mutates(),
            "read-only target access cannot mutate files or run hooks"
        );
        self.session
            .request(request, self.no_prompt, self.read_only)
    }

    pub fn read(&self, path: &Path) -> Result<Vec<u8>> {
        if !self.elevates_reads() {
            return Ok(std::fs::read(path)?);
        }
        match self.request(Request::Read {
            path: WirePath::new(path),
        })? {
            Reply::Bytes(bytes) => Ok(bytes),
            _ => bail!("unexpected privileged read response"),
        }
    }

    pub fn metadata(&self, path: &Path, follow: bool) -> Result<std::fs::Metadata> {
        if !self.elevates_reads() {
            return Ok(if follow {
                std::fs::metadata(path)?
            } else {
                std::fs::symlink_metadata(path)?
            });
        }
        #[cfg(unix)]
        {
            let mut state = self
                .session
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("privileged filesystem session lock poisoned"))?;
            match state.request(
                Request::Inspect {
                    path: WirePath::new(path),
                    follow,
                },
                self.no_prompt,
                self.read_only,
            )? {
                Reply::Descriptor => match unix::receive_file(state.stream()?) {
                    Ok(file) => file
                        .metadata()
                        .context("inspect privileged target descriptor"),
                    Err(error) => {
                        *state = unix::State::Failed;
                        Err(error.context(
                            "invalid privileged metadata descriptor; helper was not restarted",
                        ))
                    }
                },
                _ => bail!("unexpected privileged metadata response"),
            }
        }
        #[cfg(not(unix))]
        bail!("sudo mode is only supported on Unix")
    }

    pub fn read_link(&self, path: &Path) -> Result<PathBuf> {
        if !self.elevates_reads() {
            return Ok(std::fs::read_link(path)?);
        }
        match self.request(Request::ReadLink {
            path: WirePath::new(path),
        })? {
            Reply::Paths(mut paths) if paths.len() == 1 => Ok(paths.remove(0).path()),
            _ => bail!("unexpected privileged readlink response"),
        }
    }

    pub fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        if !self.elevates_reads() {
            return std::fs::read_dir(path)?
                .map(|entry| Ok(entry?.path()))
                .collect();
        }
        match self.request(Request::ReadDir {
            path: WirePath::new(path),
        })? {
            Reply::Paths(paths) => Ok(paths.iter().map(WirePath::path).collect()),
            _ => bail!("unexpected privileged directory response"),
        }
    }

    fn effect(&self, request: Request) -> Result<()> {
        match self.request(request)? {
            Reply::Done => Ok(()),
            _ => bail!("unexpected privileged filesystem response"),
        }
    }

    pub fn create_dir_all(&self, path: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot create directories"
        );
        if !self.sudo {
            return Ok(std::fs::create_dir_all(path)?);
        }
        self.effect(Request::CreateDir {
            path: WirePath::new(path),
        })
    }

    pub fn symlink(&self, source: &Path, target: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot create links"
        );
        if !self.sudo {
            return Ok(symlink::symlink_auto(source, target)?);
        }
        self.effect(Request::Symlink {
            source: WirePath::new(source),
            target: WirePath::new(target),
        })
    }

    pub fn hard_link(&self, source: &Path, target: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot create links"
        );
        if !self.sudo {
            return Ok(std::fs::hard_link(source, target)?);
        }
        self.effect(Request::HardLink {
            source: WirePath::new(source),
            target: WirePath::new(target),
        })
    }

    pub fn remove_file(&self, path: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot remove files"
        );
        if !self.sudo {
            return Ok(std::fs::remove_file(path)?);
        }
        self.effect(Request::RemoveFile {
            path: WirePath::new(path),
        })
    }

    pub fn remove_dir(&self, path: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot remove directories"
        );
        if !self.sudo {
            return Ok(std::fs::remove_dir(path)?);
        }
        self.effect(Request::RemoveDir {
            path: WirePath::new(path),
        })
    }

    pub fn write(
        &self,
        path: &Path,
        bytes: &[u8],
        mode: Option<crate::overlays::FileMode>,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot write files"
        );
        if !self.sudo {
            return replace(path, bytes, mode.map(|mode| mode.bits()));
        }
        self.effect(Request::Write {
            path: WirePath::new(path),
            bytes: bytes.to_vec(),
            mode: mode.map(|mode| mode.bits()),
        })
    }

    pub fn set_metadata(
        &self,
        path: &Path,
        permissions: &crate::overlays::Permissions,
    ) -> Result<()> {
        self.set_metadata_with_identity(path, permissions, None)
    }

    fn set_metadata_with_identity(
        &self,
        path: &Path,
        permissions: &crate::overlays::Permissions,
        expected: Option<MetadataIdentity>,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot repair metadata"
        );
        let owner = permissions
            .owner
            .as_ref()
            .map(|identity| self.session.identity(identity, false))
            .transpose()?;
        let group = permissions
            .group
            .as_ref()
            .map(|identity| self.session.identity(identity, true))
            .transpose()?;
        let mode = permissions.mode.map(|mode| mode.bits());
        if !self.sudo {
            return repair_metadata(path, mode, owner, group, expected.as_ref());
        }
        self.effect(Request::Metadata {
            path: WirePath::new(path),
            mode,
            owner,
            group,
            expected,
        })
    }

    pub(crate) fn secure_write(
        &self,
        path: &Path,
        bytes: &[u8],
        before: Option<&[u8]>,
        mode: u32,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "read-only target access cannot write secrets"
        );
        if !self.sudo {
            #[cfg(unix)]
            return unix::secure_write(path, bytes, before, mode);
            #[cfg(not(unix))]
            bail!("secure filesystem session writes require Unix");
        }
        self.effect(Request::SecureWrite {
            path: WirePath::new(path),
            bytes: bytes.to_vec(),
            before: before.map(<[u8]>::to_vec),
            mode,
        })
    }

    pub fn run_hook(
        &self,
        script: String,
        cwd: PathBuf,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        remove_env: Vec<std::ffi::OsString>,
    ) -> Result<std::process::ExitStatus> {
        anyhow::ensure!(self.sudo, "elevated hooks require explicit sudo access");
        let env = env
            .iter()
            .map(|(name, value)| {
                (
                    WirePath::new(Path::new(name)),
                    WirePath::new(Path::new(value)),
                )
            })
            .collect();
        let remove_env = remove_env
            .iter()
            .map(|name| WirePath::new(Path::new(name)))
            .collect();
        match self.request(Request::Hook {
            script,
            cwd: WirePath::new(&cwd),
            env,
            remove_env,
        })? {
            #[cfg(unix)]
            Reply::Exit(status) => {
                use std::os::unix::process::ExitStatusExt;
                Ok(std::process::ExitStatus::from_raw(status))
            }
            _ => bail!("unexpected privileged hook response"),
        }
    }
}

fn replace(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    crate::actions::fs::replace_file(path, bytes, |permissions| {
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            return std::fs::Permissions::from_mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        permissions
    })
}

pub fn resolve_identity(identity: &crate::overlays::Identity, group: bool) -> Result<u32> {
    match identity {
        crate::overlays::Identity::Id(id) => {
            anyhow::ensure!(*id != u32::MAX, "invalid ownership ID");
            Ok(*id)
        }
        crate::overlays::Identity::Name(name) => {
            if name.bytes().all(|byte| byte.is_ascii_digit()) {
                let id = name.parse::<u32>().context("invalid ownership ID")?;
                anyhow::ensure!(id != u32::MAX, "invalid ownership ID");
                return Ok(id);
            }
            #[cfg(unix)]
            return unix::resolve_name(name, group);
            #[cfg(not(unix))]
            {
                let _ = group;
                bail!("ownership policy is only supported on Unix")
            }
        }
    }
}

pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn repair_metadata(
    path: &Path,
    mode: Option<u32>,
    owner: Option<u32>,
    group: Option<u32>,
    expected: Option<&MetadataIdentity>,
) -> Result<()> {
    #[cfg(unix)]
    return unix::repair_metadata_checked(path, mode, owner, group, expected);
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        if let Some(expected) = expected {
            expected.verify(&std::fs::symlink_metadata(path)?)?;
        }
        anyhow::ensure!(
            owner.is_none() && group.is_none(),
            "ownership policy is only supported on Unix"
        );
        Ok(())
    }
}

/// Called before configuration discovery; helper requests never enter ordinary CLI dispatch.
pub fn dispatch_helper() -> Option<Result<()>> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.as_os_str() != "--over-private-sudo-helper" {
        return None;
    }
    #[cfg(unix)]
    return Some(unix::serve(args.collect()));
    #[cfg(not(unix))]
    Some(Err(anyhow::anyhow!("sudo mode is only supported on Unix")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_access_inspects_locally_even_when_the_privileged_session_is_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        // macOS temp paths can traverse /var, an alias of /private/var.
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("file");
        let link = root.join("link");
        std::fs::write(&file, "content").unwrap();
        symlink::symlink_file(&file, &link).unwrap();
        let session = Arc::new(Session::default());
        // A closed session catches accidental helper use without launching real sudo.
        session.shutdown().unwrap();
        let fs = session.target(true, false, true);
        assert!(fs.is_privileged());
        assert_eq!(fs.read_to_string(&file).unwrap(), "content");
        assert!(fs.metadata(&link, false).unwrap().is_symlink());
        assert!(fs.metadata(&link, true).unwrap().is_file());
        assert_eq!(fs.read_link(&link).unwrap(), file);
        assert_eq!(fs.read_dir(directory.path()).unwrap().len(), 2);
        assert_eq!(fs.location(&link).unwrap(), link);
        assert_eq!(fs.physical(&link).unwrap(), file);
        assert_eq!(fs.physical_destination(&link, &file).unwrap(), link);
        assert!(fs.create_dir_all(&directory.path().join("new")).is_err());
        assert!(!directory.path().join("new").exists());
    }
}
