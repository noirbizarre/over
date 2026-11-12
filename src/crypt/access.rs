//! Public logical principals and standard Age envelopes, separate from local keys.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{atomic_write, format::RepositoryKey};

const KEY_WRITE_PREFIX: &str = ".over-unlocked-key-";

#[derive(Serialize, Deserialize, Debug)]
pub struct Registry {
    pub version: u32,
    pub repository: [u8; 16],
    pub generation: [u8; 16],
    pub users: BTreeMap<String, BTreeMap<String, AccessKey>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AccessKey {
    /// A passphrase has no reusable public recipient; rotation must prompt again.
    pub recipient: Option<String>,
    pub envelope: Vec<u8>,
}

pub fn root(repo: &git2::Repository) -> Result<&Path> {
    repo.workdir()
        .context("repository encryption requires a working tree")
}

pub fn metadata_path(repo: &git2::Repository) -> Result<PathBuf> {
    Ok(root(repo)?.join(".over/crypt/access.json"))
}

pub fn configured(repo: &git2::Repository) -> Result<bool> {
    Ok(metadata_path(repo)?.try_exists()?)
}

pub fn worktrees(repo: &git2::Repository) -> Result<Vec<git2::Repository>> {
    let primary = git2::Repository::open(repo.commondir())?;
    let mut result = Vec::new();
    if !primary.is_bare() {
        result.push(primary);
    }
    for name in repo.worktrees()?.iter() {
        let worktree = repo.find_worktree(name?.context("invalid registered worktree name")?)?;
        worktree
            .validate()
            .context("registered worktree is inaccessible; repair it before locking")?;
        result.push(git2::Repository::open(worktree.path())?);
    }
    ensure!(!result.is_empty(), "repository has no accessible worktrees");
    result.sort_by_key(|repo| repo.path().to_path_buf());
    Ok(result)
}

pub(crate) fn private_dir(repo: &git2::Repository) -> PathBuf {
    repo.commondir().join("over-crypt")
}

fn check_real(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && metadata.is_dir() == directory
            && (directory || metadata.is_file()),
        "encryption state must use real files and directories"
    );
    Ok(())
}

fn prepare_private(repo: &git2::Repository) -> Result<PathBuf> {
    let path = private_dir(repo);
    if !path.try_exists()? {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&path)?;
        #[cfg(windows)]
        windows_acl(&path, true)?;
    }
    check_private(repo, &path, true)?;
    Ok(path)
}

fn check_private(repo: &git2::Repository, path: &Path, directory: bool) -> Result<()> {
    check_real(path, directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.mode() & 0o077 == 0,
            "unlocked key storage is accessible to other users; restrict its permissions"
        );
        ensure!(
            metadata.uid() == fs::metadata(repo.commondir())?.uid(),
            "unlocked key storage has unexpected ownership"
        );
        ensure!(
            directory || metadata.nlink() == 1,
            "unlocked key must not have hard links"
        );
    }
    #[cfg(not(unix))]
    let _ = repo;
    #[cfg(windows)]
    {
        ensure!(
            directory || winapi_util::file::information(File::open(path)?)?.number_of_links() == 1,
            "unlocked key storage must not have hard links"
        );
        windows_acl(path, false)?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn windows_acl(path: &Path, initialize: bool) -> Result<()> {
    // Use the OS ACL implementation; readonly attributes do not provide key confidentiality.
    let script = if initialize {
        "$ErrorActionPreference='Stop'; $p=$env:OVER_CRYPT_ACL_PATH; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=Get-Acl -LiteralPath $p; $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false); foreach($r in @($acl.Access)){$acl.RemoveAccessRuleSpecific($r)}; $flags=if((Get-Item -LiteralPath $p).PSIsContainer){'ContainerInherit,ObjectInherit'}else{'None'}; $r=[System.Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl',$flags,'None','Allow'); $acl.AddAccessRule($r); Set-Acl -LiteralPath $p -AclObject $acl"
    } else {
        "$ErrorActionPreference='Stop'; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $acl=Get-Acl -LiteralPath $env:OVER_CRYPT_ACL_PATH; if($acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid.Value){exit 1}; foreach($r in $acl.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])){if($r.AccessControlType -eq 'Allow' -and $r.IdentityReference.Value -ne $sid.Value){exit 1}}"
    };
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("OVER_CRYPT_ACL_PATH", path)
        .output()
        .with_context(|| {
            format!(
                "could not {} private ACL at '{}'; PowerShell is required on Windows",
                if initialize { "initialize" } else { "inspect" },
                path.display()
            )
        })?;
    // Only public ACL diagnostics are requested; escape terminal controls and bound subprocess output.
    let stderr: String = String::from_utf8_lossy(&result.stderr)
        .chars()
        .take(2048)
        .flat_map(char::escape_default)
        .collect();
    ensure!(
        result.status.success(),
        "could not {} private ACL at '{}': status {}; stderr: {}",
        if initialize { "initialize" } else { "inspect" },
        path.display(),
        result.status,
        stderr
    );
    Ok(())
}

/// Sentinel inode is never replaced, so atomic data writes cannot split locks.
pub fn guard(repo: &git2::Repository, exclusive: bool) -> Result<File> {
    let file = raw_guard(repo, exclusive)?;
    ensure!(
        !private_dir(repo).join("transaction.json").try_exists()?,
        "interrupted crypt transaction; run over crypt recover before continuing"
    );
    Ok(file)
}

pub(crate) fn raw_guard(repo: &git2::Repository, exclusive: bool) -> Result<File> {
    let directory = prepare_private(repo)?;
    let path = directory.join("operation.lock");
    // Reject special files before opening: opening a hostile FIFO can block forever.
    match fs::symlink_metadata(&path) {
        Ok(_) => check_private(repo, &path, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    let existed = path.try_exists()?;
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    #[cfg(windows)]
    if !existed {
        windows_acl(&path, true)?;
    }
    check_private(repo, &path, false)?;
    if exclusive {
        file.lock()?;
        // Only exclusive lifecycle/recovery operations can know these writers have exited.
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(KEY_WRITE_PREFIX)
            {
                check_private(repo, &entry.path(), false)?;
                ensure!(
                    entry.metadata()?.len() <= 96,
                    "unexpected interrupted key-write format; inspect private administrative state before continuing"
                );
                fs::remove_file(entry.path())?;
            }
        }
        #[cfg(unix)]
        File::open(&directory)?.sync_all()?;
    } else {
        file.lock_shared()?;
    }
    Ok(file)
}

pub fn load(repo: &git2::Repository) -> Result<Registry> {
    let path = metadata_path(repo)?;
    for parent in [root(repo)?.join(".over"), root(repo)?.join(".over/crypt")] {
        check_real(&parent, true)?;
    }
    check_real(&path, false)?;
    ensure!(
        fs::metadata(&path)?.len() <= 16 * 1024 * 1024,
        "repository access metadata exceeds the supported size limit"
    );
    let registry: Registry = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("invalid repository access metadata"))?;
    ensure!(
        registry.version == 1,
        "unsupported repository access metadata version"
    );
    ensure!(
        !registry.users.is_empty(),
        "repository has no recovery user"
    );
    ensure!(
        registry.users.values().any(|keys| !keys.is_empty()),
        "repository has no recovery key"
    );
    for (user, keys) in &registry.users {
        validate_name(user)?;
        for (name, key) in keys {
            validate_name(name)?;
            ensure!(
                key.envelope.len() <= 16 * 1024,
                "repository key envelope exceeds the supported size limit"
            );
        }
    }
    Ok(registry)
}

pub(crate) fn save(repo: &git2::Repository, registry: &Registry) -> Result<()> {
    let directory = root(repo)?.join(".over");
    if directory.try_exists()? {
        check_real(&directory, true)?;
    }
    let directory = directory.join("crypt");
    if directory.try_exists()? {
        check_real(&directory, true)?;
    }
    let attributes = root(repo)?.join(".over/.gitattributes");
    if !attributes.try_exists()? {
        atomic_write(
            &attributes,
            b"crypt/** -filter -text\n.gitattributes -filter\nsecrets.yaml filter=over -text\n",
        )?;
    }
    let bytes = serde_json::to_vec_pretty(registry)?;
    // A writer must not publish a registry that its own bounded reader rejects.
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "repository access metadata exceeds the supported size limit"
    );
    atomic_write(&metadata_path(repo)?, &bytes)
}

pub fn local_key(repo: &git2::Repository) -> Result<RepositoryKey> {
    let directory = private_dir(repo);
    check_private(repo, &directory, true).context("repository is locked; run over unlock")?;
    ensure!(
        !directory.join("transaction.json").try_exists()?,
        "interrupted crypt transaction; run over crypt recover"
    );
    let path = directory.join("key");
    check_private(repo, &path, false).context("repository is locked; run over unlock")?;
    let key = RepositoryKey::decode(read_key_bytes(File::open(path)?)?.as_ref())?;
    let registry = load(repo)?;
    ensure!(
        key.repository == registry.repository && key.generation == registry.generation,
        "local key is stale or belongs to another repository; run over unlock for this generation"
    );
    Ok(key)
}

pub(crate) fn recovery_key(repo: &git2::Repository) -> Result<RepositoryKey> {
    let path = private_dir(repo).join("key");
    check_private(repo, &path, false)?;
    RepositoryKey::decode(read_key_bytes(File::open(path)?)?.as_ref())
}

pub fn local_key_missing(repo: &git2::Repository) -> Result<bool> {
    Ok(!private_dir(repo).join("key").try_exists()?)
}

pub fn remember_identity(repo: &git2::Repository, recipient: &str, path: &Path) -> Result<()> {
    let _guard = guard(repo, true)?;
    let file = private_dir(repo).join("identities.json");
    let mut identities: BTreeMap<String, PathBuf> = match fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid local identity hints"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    identities.insert(recipient.to_owned(), fs::canonicalize(path)?);
    atomic_write(&file, &serde_json::to_vec(&identities)?)
}

pub fn identity_hint(repo: &git2::Repository, recipient: &str) -> Result<Option<PathBuf>> {
    let file = private_dir(repo).join("identities.json");
    if !file.try_exists()? {
        return Ok(None);
    }
    check_private(repo, &file, false)?;
    let identities: BTreeMap<String, PathBuf> = serde_json::from_slice(&fs::read(file)?)
        .map_err(|_| anyhow::anyhow!("invalid local identity hints"))?;
    Ok(identities.get(recipient).cloned())
}

pub(crate) fn install(repo: &git2::Repository, key: &RepositoryKey) -> Result<()> {
    let directory = prepare_private(repo)?;
    let path = directory.join("key");
    // Never replace unsafe state and thereby hide evidence of a linked or shared key.
    match fs::symlink_metadata(&path) {
        Ok(_) => check_private(repo, &path, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    super::atomic_write_named(&path, &key.encode(), KEY_WRITE_PREFIX)
}

pub fn forget(repo: &git2::Repository) -> Result<()> {
    let _guard = guard(repo, true)?;
    remove_key(repo)
}

/// Caller holds the exclusive operation guard, including during refresh rollback.
pub(crate) fn remove_key(repo: &git2::Repository) -> Result<()> {
    fs::remove_file(private_dir(repo).join("key")).context("repository is already locked")?;
    #[cfg(unix)]
    File::open(private_dir(repo))?.sync_all()?;
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "use 1–128 ASCII letters, digits, hyphens or underscores for user and key names"
    );
    Ok(())
}

pub fn envelope(
    key: &RepositoryKey,
    recipient: Option<&str>,
    passphrase: Option<SecretString>,
) -> Result<AccessKey> {
    let (encryptor, recipient) = if let Some(recipient) = recipient {
        let recipient: age::x25519::Recipient = recipient
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid standard Age software recipient"))?;
        (
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?,
            Some(recipient.to_string()),
        )
    } else {
        (
            age::Encryptor::with_user_passphrase(passphrase.context("passphrase required")?),
            None,
        )
    };
    let mut output = Vec::new();
    let mut writer = encryptor.wrap_output(&mut output)?;
    // Age 0.12.1 copies plaintext into StreamWriter's private Vec chunk, which is
    // not zeroized on finish/drop. Our encoded input is wiped, but that external
    // allocation cannot be wiped through Age's public API (including on error).
    writer.write_all(&key.encode())?;
    writer.finish()?;
    Ok(AccessKey {
        recipient,
        envelope: output,
    })
}

pub fn verify_passphrase(
    access: &AccessKey,
    passphrase: SecretString,
    key: &RepositoryKey,
) -> Result<()> {
    let identity = age::scrypt::Identity::new(passphrase);
    let mut reader = age::Decryptor::new(access.envelope.as_slice())?
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| anyhow::anyhow!("retained key passphrase is incorrect; rotation aborted"))?;
    let bytes = read_key_bytes(&mut reader)
        .map_err(|_| anyhow::anyhow!("retained key envelope is corrupt"))?;
    let expected = key.encode();
    ensure!(
        bytes.len() == expected.len()
            && bytes
                .iter()
                .zip(expected.iter())
                .fold(0u8, |difference, (a, b)| difference | (a ^ b))
                == 0,
        "retained key envelope does not wrap the current repository key"
    );
    Ok(())
}

fn read_key_bytes(mut reader: impl Read) -> Result<Zeroizing<[u8; 96]>> {
    let mut bytes = Zeroizing::new([0; 96]);
    reader
        .read_exact(bytes.as_mut())
        .map_err(|_| anyhow::anyhow!("invalid or corrupt repository key"))?;
    // Reading beyond the exact key length forces Age to authenticate EOF, including
    // a separate final chunk; merely filling 96 bytes would accept truncated streams.
    let mut extra = Zeroizing::new([0; 1]);
    ensure!(
        reader
            .read(extra.as_mut())
            .map_err(|_| anyhow::anyhow!("invalid or corrupt repository key"))?
            == 0,
        "repository key exceeds the supported size"
    );
    Ok(bytes)
}

pub fn add_key(
    repo: &git2::Repository,
    user: &str,
    name: &str,
    recipient: Option<&str>,
    passphrase: Option<SecretString>,
    new_user: bool,
) -> Result<()> {
    validate_name(user)?;
    validate_name(name)?;
    let _guard = guard(repo, true)?;
    let (mut registry, key) = if configured(repo)? {
        (load(repo)?, local_key(repo)?)
    } else {
        ensure!(new_user, "initialize encryption with over user add");
        let key = RepositoryKey::generate()?;
        (
            Registry {
                version: 1,
                repository: key.repository,
                generation: key.generation,
                users: BTreeMap::new(),
            },
            key,
        )
    };
    ensure!(
        registry.users.contains_key(user) != new_user,
        "user already exists or is missing"
    );
    let access = envelope(&key, recipient, passphrase)?;
    if let Some(recipient) = &access.recipient {
        // Duplicate public identities would let a revoked key unwrap a retained alias's envelope.
        ensure!(
            !registry
                .users
                .values()
                .flat_map(|keys| keys.values())
                .any(|key| key.recipient.as_ref() == Some(recipient)),
            "Age recipient is already attached to a key; reuse that key rather than creating an unrevocable alias"
        );
    }
    let keys = registry.users.entry(user.to_owned()).or_default();
    ensure!(!keys.contains_key(name), "key already exists");
    keys.insert(name.to_owned(), access);
    // Fail on config contention before publishing any access metadata or key.
    configure_filters(repo)?;
    let path = metadata_path(repo)?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    // The complete public registry includes envelopes, so a user cannot persist empty.
    save(repo, &registry)?;
    if let Err(error) = install(repo, &key) {
        // A parent fsync can fail after rename. Keep matching metadata when the
        // new key is already visible, rather than rolling back only half the pair.
        let installed = private_dir(repo).join("key");
        if check_private(repo, &installed, false).is_ok()
            && File::open(&installed)
                .ok()
                .and_then(|file| read_key_bytes(file).ok())
                .is_some_and(|bytes| bytes[..] == key.encode()[..])
        {
            return Err(error);
        }
        // Installation publishes atomically, so a pre-publication failure leaves the
        // previous key intact and its matching public registry can be restored.
        let rollback = match previous {
            Some(bytes) => atomic_write(&path, &bytes),
            None => fs::remove_file(&path).map_err(anyhow::Error::from),
        };
        rollback.with_context(|| {
            format!("key installation failed ({error}); access metadata rollback also failed")
        })?;
        #[cfg(unix)]
        File::open(path.parent().context("access metadata has no parent")?)?.sync_all()?;
        return Err(error);
    }
    Ok(())
}

pub fn unlock(
    repo: &git2::Repository,
    identity: Option<&Path>,
    passphrase: Option<SecretString>,
    user: Option<&str>,
    name: Option<&str>,
) -> Result<()> {
    super::policy::unlock(repo, identity, passphrase, user, name)
}

/// Unwrap credentials only; the caller holds the operation guard and owns installation.
pub(crate) fn unwrap_key(
    repo: &git2::Repository,
    identity: Option<&Path>,
    passphrase: Option<SecretString>,
    user: Option<&str>,
    name: Option<&str>,
) -> Result<RepositoryKey> {
    let registry = load(repo)?;
    let identities = identity
        .map(|path| age::IdentityFile::from_file(path.to_string_lossy().into_owned()))
        .transpose()
        .map_err(|_| anyhow::anyhow!("could not read Age identity file"))?;
    let identities = identities
        .map(|file| file.into_identities())
        .transpose()
        .map_err(|_| anyhow::anyhow!("could not load Age identities"))?
        .unwrap_or_default();
    let mut damaged = false;
    for (username, keys) in &registry.users {
        if user.is_some_and(|user| user != username) {
            continue;
        }
        for (keyname, access) in keys {
            if name.is_some_and(|name| name != keyname) {
                continue;
            }
            let decryptor = match age::Decryptor::new(access.envelope.as_slice()) {
                Ok(decryptor) => decryptor,
                Err(_) => {
                    damaged = true;
                    continue;
                }
            };
            let password = passphrase.clone().map(age::scrypt::Identity::new);
            let mut all: Vec<&dyn age::Identity> = identities
                .iter()
                .map(|identity| identity.as_ref() as &dyn age::Identity)
                .collect();
            if let Some(password) = &password {
                all.push(password);
            }
            let Ok(mut reader) = decryptor.decrypt(all.into_iter()) else {
                continue;
            };
            let key = match read_key_bytes(&mut reader)
                .and_then(|plaintext| RepositoryKey::decode(plaintext.as_ref()))
            {
                Ok(key) => key,
                Err(_) => {
                    damaged = true;
                    continue;
                }
            };
            if key.repository != registry.repository || key.generation != registry.generation {
                damaged = true;
                continue;
            }
            return Ok(key);
        }
    }
    // An independently usable trusted key must remain usable if another envelope is damaged.
    ensure!(
        !damaged,
        "no selected Age key unlocked the repository; some envelopes are corrupt, unsupported or generation-mismatched; restore trusted access metadata or select a remaining recovery key"
    );
    anyhow::bail!("no selected Age key could unlock this repository")
}

pub fn detach(repo: &git2::Repository, user: &str, name: &str) -> Result<()> {
    let _guard = guard(repo, true)?;
    let mut registry = load(repo)?;
    let count: usize = registry.users.values().map(BTreeMap::len).sum();
    ensure!(count > 1, "cannot detach the only recovery key");
    ensure!(
        registry
            .users
            .get_mut(user)
            .and_then(|keys| keys.remove(name))
            .is_some(),
        "unknown user/key"
    );
    save(repo, &registry)
}

pub fn delete_user(repo: &git2::Repository, user: &str) -> Result<()> {
    let _guard = guard(repo, true)?;
    let mut registry = load(repo)?;
    ensure!(
        registry.users.get(user).is_some_and(BTreeMap::is_empty),
        "delete requires an existing user with no keys; detach or revoke explicitly"
    );
    registry.users.remove(user);
    save(repo, &registry)
}

pub fn configure_filters(repo: &git2::Repository) -> Result<()> {
    let commands = filter_commands()?;
    let path = repo.commondir().join("config");
    check_real(&path, false)?;
    let mut lock = crate::actions::git::index::IndexLock::acquire(&path)
        .context("could not lock local Git config for encryption filters")?;
    // A common config update cannot override conflicting worktree-local filters.
    let config = repo.config()?;
    let worktree_config = match config.get_bool("extensions.worktreeConfig") {
        Ok(value) => value,
        Err(error) if error.code() == git2::ErrorCode::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if worktree_config {
        for worktree in worktrees(repo)? {
            let path = worktree.path().join("config.worktree");
            if !path.try_exists()? {
                continue;
            }
            let config = git2::Config::open(&path)?;
            for (name, command) in &commands {
                match config.get_string(name) {
                    Ok(value) => ensure!(
                        value == *command,
                        "worktree-local config overrides encryption filters; resolve that override before unlocking"
                    ),
                    Err(error) if error.code() == git2::ErrorCode::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
            }
            match config.get_bool("filter.over.required") {
                Ok(value) => ensure!(
                    value,
                    "worktree-local config disables required encryption filters"
                ),
                Err(error) if error.code() == git2::ErrorCode::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    // Keep the copy beside the real config so relative include paths keep meaning.
    let mut copy = tempfile::NamedTempFile::new_in(repo.commondir())?;
    copy.write_all(&fs::read(&path)?)?;
    let mut config = git2::Config::open(copy.path())?;
    for (name, command) in &commands {
        config.set_str(name, command)?;
    }
    config.set_bool("filter.over.required", true)?;
    // Included later values must not silently shadow commands written to the copy.
    for (name, command) in &commands {
        ensure!(
            config.get_string(name)? == *command,
            "included Git config overrides encryption filters"
        );
    }
    ensure!(
        config.get_bool("filter.over.required")?,
        "included Git config disables required encryption filters"
    );
    drop(config);
    lock.publish(&path, &fs::read(copy.path())?)?;
    #[cfg(unix)]
    File::open(repo.commondir())?.sync_all()?;
    Ok(())
}

pub fn filter_commands() -> Result<[(&'static str, String); 3]> {
    // Git evaluates filter commands through a shell, including executable paths.
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .context("Over executable path is not UTF-8")?;
    let executable = format!("'{}'", executable.replace('\'', "'\\''"));
    Ok([
        (
            "filter.over.clean",
            format!("{executable} git filter clean"),
        ),
        (
            "filter.over.smudge",
            format!("{executable} git filter smudge"),
        ),
        (
            "filter.over.process",
            format!("{executable} git filter-process"),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (tempfile::TempDir, git2::Repository) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        (directory, repo)
    }

    fn add(repo: &git2::Repository, user: &str) -> Result<()> {
        let identity = age::x25519::Identity::generate();
        add_key(
            repo,
            user,
            "laptop",
            Some(&identity.to_public().to_string()),
            None,
            true,
        )
    }

    #[test]
    fn a_config_lock_prevents_new_registry_and_key_publication() {
        let (_directory, repo) = repository();
        let before = fs::read(repo.commondir().join("config")).unwrap();
        fs::write(repo.commondir().join("config.lock"), b"other writer").unwrap();
        assert!(add(&repo, "alice").is_err());
        assert!(!metadata_path(&repo).unwrap().exists());
        assert!(local_key_missing(&repo).unwrap());
        assert_eq!(fs::read(repo.commondir().join("config")).unwrap(), before);
        assert_eq!(
            fs::read(repo.commondir().join("config.lock")).unwrap(),
            b"other writer"
        );
    }

    #[test]
    fn a_config_lock_preserves_an_existing_registry_and_key() {
        let (_directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let metadata = fs::read(metadata_path(&repo).unwrap()).unwrap();
        let key = Zeroizing::new(fs::read(private_dir(&repo).join("key")).unwrap());
        let config = fs::read(repo.commondir().join("config")).unwrap();
        fs::write(repo.commondir().join("config.lock"), b"other writer").unwrap();
        assert!(add(&repo, "bob").is_err());
        assert_eq!(fs::read(metadata_path(&repo).unwrap()).unwrap(), metadata);
        assert!(fs::read(private_dir(&repo).join("key")).unwrap() == *key);
        assert_eq!(fs::read(repo.commondir().join("config")).unwrap(), config);
    }

    #[test]
    fn an_obstructed_install_preserves_the_old_key_and_registry() {
        let (_directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let path = private_dir(&repo).join("key");
        let old = private_dir(&repo).join("retained-key");
        fs::rename(&path, &old).unwrap();
        fs::create_dir(&path).unwrap();
        let bytes = Zeroizing::new(fs::read(&old).unwrap());
        let metadata = fs::read(metadata_path(&repo).unwrap()).unwrap();
        assert!(install(&repo, &RepositoryKey::generate().unwrap()).is_err());
        assert!(fs::read(&old).unwrap() == *bytes);
        assert!(path.is_dir());
        assert_eq!(fs::read(metadata_path(&repo).unwrap()).unwrap(), metadata);
    }

    #[test]
    fn an_initial_install_failure_rolls_back_the_new_registry() {
        let (_directory, repo) = repository();
        let _guard = guard(&repo, true).unwrap();
        fs::create_dir(private_dir(&repo).join("key")).unwrap();
        drop(_guard);
        assert!(add(&repo, "alice").is_err());
        assert!(!metadata_path(&repo).unwrap().exists());
        assert!(private_dir(&repo).join("key").is_dir());
    }

    #[test]
    fn a_late_config_edit_failure_does_not_publish_earlier_filter_edits() {
        let (_directory, repo) = repository();
        let path = repo.commondir().join("config");
        let mut bytes = fs::read(&path).unwrap();
        // libgit2 refuses a single-value edit of duplicate values, after clean
        // and smudge have already been changed in the unpublished copy.
        bytes.extend_from_slice(b"\n[filter \"over\"]\n process = first\n process = second\n");
        fs::write(&path, &bytes).unwrap();
        assert!(configure_filters(&repo).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!repo.commondir().join("config.lock").exists());
    }

    #[test]
    fn unwrapping_credentials_does_not_install_keys_or_configure_filters() {
        use age::secrecy::ExposeSecret as _;
        let (directory, repo) = repository();
        let identity = age::x25519::Identity::generate();
        let identity_path = directory.path().join("identity");
        fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();
        let key = RepositoryKey::generate().unwrap();
        let access = envelope(&key, Some(&identity.to_public().to_string()), None).unwrap();
        let registry = Registry {
            version: 1,
            repository: key.repository,
            generation: key.generation,
            users: BTreeMap::from([("alice".into(), BTreeMap::from([("laptop".into(), access)]))]),
        };
        save(&repo, &registry).unwrap();
        let config = fs::read(repo.commondir().join("config")).unwrap();
        let _guard = guard(&repo, true).unwrap();
        let unwrapped = unwrap_key(
            &repo,
            Some(&identity_path),
            None,
            Some("alice"),
            Some("laptop"),
        )
        .unwrap();
        assert!(unwrapped.encode() == key.encode());
        assert!(local_key_missing(&repo).unwrap());
        assert_eq!(fs::read(repo.commondir().join("config")).unwrap(), config);
        assert!(unwrap_key(&repo, Some(&identity_path), None, Some("bob"), None).is_err());
        assert!(unwrap_key(&repo, Some(&identity_path), None, None, Some("missing")).is_err());
    }

    #[rstest::rstest]
    #[case(95, false, false)]
    #[case(96, false, true)]
    #[case(97, false, false)]
    #[case(96, true, false)]
    fn envelope_reads_require_exact_length_and_authenticated_eof(
        #[case] length: usize,
        #[case] corrupt: bool,
        #[case] accepted: bool,
    ) {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .unwrap();
        let mut ciphertext = Vec::new();
        let mut writer = encryptor.wrap_output(&mut ciphertext).unwrap();
        writer.write_all(&vec![0; length]).unwrap();
        writer.finish().unwrap();
        if corrupt {
            ciphertext.pop();
        }
        let reader = age::Decryptor::new(ciphertext.as_slice())
            .unwrap()
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap();
        assert_eq!(read_key_bytes(reader).is_ok(), accepted);
    }

    #[test]
    fn removing_a_key_under_an_existing_guard_does_not_relock() {
        let (_directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let _guard = guard(&repo, true).unwrap();
        remove_key(&repo).unwrap();
        assert!(local_key_missing(&repo).unwrap());
    }

    #[rstest::rstest]
    #[case(true)]
    #[case(false)]
    fn a_stale_or_foreign_key_is_rejected(#[case] stale: bool) {
        let (_directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let mut key = RepositoryKey::generate().unwrap();
        let registry = load(&repo).unwrap();
        if stale {
            key.repository = registry.repository;
        }
        install(&repo, &key).unwrap();
        assert!(local_key(&repo).is_err());
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case("operation.lock", false)]
    #[case("key", false)]
    #[case("", true)]
    fn unsafe_permissions_are_rejected(#[case] name: &str, #[case] directory: bool) {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let path = if name.is_empty() {
            private_dir(&repo)
        } else {
            private_dir(&repo).join(name)
        };
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if directory { 0o755 } else { 0o644 }),
        )
        .unwrap();
        assert!(check_private(&repo, &path, directory).is_err());
        if name != "key" {
            assert!(guard(&repo, true).is_err());
        }
        if name != "operation.lock" {
            assert!(local_key(&repo).is_err());
        }
    }

    #[cfg(any(unix, windows))]
    #[rstest::rstest]
    #[case("operation.lock")]
    #[case("key")]
    fn hard_linked_private_files_are_rejected(#[case] name: &str) {
        let (directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let before = Zeroizing::new(fs::read(private_dir(&repo).join(name)).unwrap());
        fs::hard_link(
            private_dir(&repo).join(name),
            directory.path().join("alias"),
        )
        .unwrap();
        assert!(check_private(&repo, &private_dir(&repo).join(name), false).is_err());
        if name == "key" {
            assert!(local_key(&repo).is_err());
            assert!(install(&repo, &RepositoryKey::generate().unwrap()).is_err());
            assert!(fs::read(private_dir(&repo).join(name)).unwrap() == *before);
            assert!(fs::read(directory.path().join("alias")).unwrap() == *before);
        } else {
            assert!(guard(&repo, true).is_err());
        }
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case("operation.lock")]
    #[case("key")]
    #[case("")]
    fn symlinked_private_state_is_rejected(#[case] name: &str) {
        use std::os::unix::fs::symlink;
        let (directory, repo) = repository();
        add(&repo, "alice").unwrap();
        let path = if name.is_empty() {
            private_dir(&repo)
        } else {
            private_dir(&repo).join(name)
        };
        let target = directory.path().join("target");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(check_private(&repo, &path, name.is_empty()).is_err());
        if name == "key" {
            assert!(local_key(&repo).is_err());
            assert!(install(&repo, &RepositoryKey::generate().unwrap()).is_err());
        } else {
            assert!(guard(&repo, true).is_err());
        }
    }

    #[test]
    fn filters_publish_together_and_preserve_unrelated_configuration() {
        let (_directory, repo) = repository();
        repo.config()
            .unwrap()
            .set_str("over.unrelated", "retained")
            .unwrap();
        configure_filters(&repo).unwrap();
        let config = repo.config().unwrap();
        assert_eq!(config.get_string("over.unrelated").unwrap(), "retained");
        for (name, value) in filter_commands().unwrap() {
            assert_eq!(config.get_string(name).unwrap(), value);
        }
        assert!(config.get_bool("filter.over.required").unwrap());
    }

    #[test]
    fn worktree_filter_overrides_fail_without_modifying_local_config() {
        let (_directory, repo) = repository();
        repo.config()
            .unwrap()
            .set_bool("extensions.worktreeConfig", true)
            .unwrap();
        fs::write(
            repo.path().join("config.worktree"),
            b"[filter \"over\"]\n clean = wrong\n",
        )
        .unwrap();
        let before = fs::read(repo.commondir().join("config")).unwrap();
        assert!(configure_filters(&repo).is_err());
        assert_eq!(fs::read(repo.commondir().join("config")).unwrap(), before);
    }

    #[test]
    fn independent_guards_share_one_sentinel_across_key_replacement() {
        let (_directory, repo) = repository();
        let first = guard(&repo, true).unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(private_dir(&repo).join("operation.lock"))
            .unwrap();
        assert!(second.try_lock().is_err());
        install(&repo, &RepositoryKey::generate().unwrap()).unwrap();
        assert!(second.try_lock().is_err());
        drop(first);
        second.try_lock().unwrap();
    }

    #[test]
    fn conditional_config_cannot_make_filter_installation_report_a_false_success() {
        let (_directory, repo) = repository();
        repo.config()
            .unwrap()
            .set_bool("filter.over.required", false)
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("filter.over.process", "old-filter")
            .unwrap();
        let include = repo.path().join("conditional.conf");
        fs::write(
            &include,
            "[filter \"over\"]\n required = false\n process = false\n",
        )
        .unwrap();
        let config = repo.commondir().join("config");
        let mut bytes = fs::read(&config).unwrap();
        let gitdir = repo.path().to_string_lossy().replace('\\', "/");
        bytes.extend_from_slice(
            format!("\n[includeIf \"gitdir:{gitdir}\"]\n path = conditional.conf\n").as_bytes(),
        );
        fs::write(&config, &bytes).unwrap();
        if configure_filters(&repo).is_ok() {
            assert_eq!(
                super::super::policy::git(
                    &repo,
                    &["config", "--type=bool", "--get", "filter.over.required"]
                )
                .unwrap(),
                b"true\n"
            );
        } else {
            assert_eq!(fs::read(config).unwrap(), bytes);
        }
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(48)]
    #[case(96)]
    fn abandoned_key_writes_are_removed_only_under_exclusive_lifecycle_ownership(
        #[case] length: usize,
    ) {
        let (_directory, repo) = repository();
        add(&repo, "owner").unwrap();
        let before = local_key(&repo).unwrap().encode();
        let stale = private_dir(&repo).join(format!("{KEY_WRITE_PREFIX}interrupted"));
        atomic_write(&stale, &vec![0; length]).unwrap();
        let unrelated = private_dir(&repo).join("leave-this-note");
        atomic_write(&unrelated, b"independent administrative note").unwrap();
        let shared = guard(&repo, false).unwrap();
        assert!(
            stale.exists(),
            "shared consumers must not delete another writer's active preparation"
        );
        drop(shared);
        let exclusive = guard(&repo, true).unwrap();
        assert!(!stale.exists());
        assert!(unrelated.exists());
        assert!(
            Zeroizing::new(fs::read(private_dir(&repo).join("key")).unwrap()).as_slice()
                == before.as_slice()
        );
        drop(exclusive);
    }

    #[test]
    fn worktree_configuration_without_filter_overrides_keeps_common_filter_setup_usable() {
        let (_directory, repo) = repository();
        repo.config()
            .unwrap()
            .set_bool("extensions.worktreeConfig", true)
            .unwrap();
        fs::write(
            repo.path().join("config.worktree"),
            "[over]\n unrelated = retained\n",
        )
        .unwrap();
        configure_filters(&repo).unwrap();
        assert!(
            repo.config()
                .unwrap()
                .get_bool("filter.over.required")
                .unwrap()
        );
        assert_eq!(
            super::super::policy::git(&repo, &["config", "--get", "over.unrelated"]).unwrap(),
            b"retained\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn foreign_owned_keys_are_rejected_when_privileged_fixture_creation_is_available() {
        use std::os::unix::fs::{MetadataExt, chown};
        let (_directory, repo) = repository();
        if fs::metadata(repo.commondir()).unwrap().uid() != 0 {
            return;
        }
        add(&repo, "owner").unwrap();
        let key = private_dir(&repo).join("key");
        chown(&key, Some(65534), Some(65534)).unwrap();
        assert!(local_key(&repo).is_err());
        assert!(install(&repo, &RepositoryKey::generate().unwrap()).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_cannot_redirect_private_key_storage() {
        let (_directory, repo) = repository();
        add(&repo, "owner").unwrap();
        let private = private_dir(&repo);
        let actual = repo.commondir().join("moved-private");
        fs::rename(&private, &actual).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&private)
            .arg(&actual)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(guard(&repo, false).is_err());
        assert!(actual.join("key").exists());
        fs::remove_dir(private).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn broad_key_acls_are_rejected_without_repairing_or_replacing_the_key() {
        let (_directory, repo) = repository();
        add(&repo, "owner").unwrap();
        let key = private_dir(&repo).join("key");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $a=Get-Acl -LiteralPath $env:OVER_KEY_TEST; $s=[System.Security.Principal.SecurityIdentifier]::new('S-1-1-0'); $a.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new($s,'Read','Allow')); Set-Acl -LiteralPath $env:OVER_KEY_TEST -AclObject $a"])
            .env("OVER_KEY_TEST", &key).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(local_key(&repo).is_err());
        assert!(install(&repo, &RepositoryKey::generate().unwrap()).is_err());
        windows_acl(&key, true).unwrap();
    }
}
