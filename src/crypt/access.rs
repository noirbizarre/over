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
    windows_acl(path, false)?;
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
        .context("could not inspect private key ACL; PowerShell is required on Windows")?;
    ensure!(
        result.status.success(),
        "unlocked key storage has unsafe ownership or ACL permissions"
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
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "encryption lock must not be a symlink"
    );
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
    let bytes = Zeroizing::new(fs::read(path)?);
    let key = RepositoryKey::decode(&bytes)?;
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
    RepositoryKey::decode(&Zeroizing::new(fs::read(path)?))
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
    atomic_write(&directory.join("key"), &key.encode())
}

pub fn forget(repo: &git2::Repository) -> Result<()> {
    let _guard = guard(repo, true)?;
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
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .read_to_end(&mut bytes)
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
    // The complete public registry includes envelopes, so a user cannot persist empty.
    save(repo, &registry)?;
    install(repo, &key)?;
    configure_filters(repo)?;
    Ok(())
}

pub fn unlock(
    repo: &git2::Repository,
    identity: Option<&Path>,
    passphrase: Option<SecretString>,
    user: Option<&str>,
    name: Option<&str>,
) -> Result<()> {
    let _guard = guard(repo, true)?;
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
            let mut plaintext = Zeroizing::new(Vec::new());
            if reader.read_to_end(&mut plaintext).is_err() {
                damaged = true;
                continue;
            }
            let key = match RepositoryKey::decode(&plaintext) {
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
            install(repo, &key)?;
            configure_filters(repo)?;
            return Ok(());
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
    let mut config = repo.config()?.open_level(git2::ConfigLevel::Local)?;
    for (name, command) in commands {
        config.set_str(name, &command)?;
    }
    config.set_bool("filter.over.required", true)?;
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
