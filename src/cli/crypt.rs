use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Result, ensure};
use clap::{Args, Subcommand};

use crate::crypt::{access, policy};

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Protect paths or Git attribute patterns and restage existing content
    Add {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Stop protecting paths and explicitly stage their plaintext
    Remove {
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Inspect canonical policy and protected stored content
    List,
    /// Roll back an interrupted encryption transaction
    Recover,
    /// Migrate legacy Age files and native secrets explicitly
    Migrate,
    /// Run the repository encryption filter
    #[command(subcommand)]
    Filter(FilterCommand),
    /// Run the long-lived Git encryption filter
    FilterProcess,
    /// Convert an encrypted Git file for diff display
    Textconv { file: PathBuf },
    /// Render an authenticated native-secret patch for Git
    Diff { args: Vec<std::ffi::OsString> },
}

#[derive(Subcommand, Debug)]
pub enum FilterCommand {
    /// Encrypt protected content for Git storage
    Clean { path: Option<PathBuf> },
    /// Decrypt protected content for the working tree
    Smudge { path: Option<PathBuf> },
}

#[derive(Args, Clone, Debug)]
pub struct Unlock {
    /// Read a standard Age identity file
    #[arg(short = 'i', long)]
    pub identity: Vec<PathBuf>,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub key: Option<String>,
}

#[derive(Args, Clone, Debug)]
pub struct NewKey {
    pub user: String,
    /// Name the new access key
    #[arg(conflicts_with_all = ["name_option", "name_prefix"])]
    pub name: Option<String>,
    /// Name the new access key (alternative to positional NAME)
    #[arg(long = "name", value_name = "NAME", conflicts_with = "name_prefix")]
    pub name_option: Option<String>,
    /// Name imported recipients PREFIX-1, PREFIX-2, in input order
    #[arg(long)]
    pub name_prefix: Option<String>,
    /// Wrap with an existing standard Age public recipient
    #[arg(short = 'r', long, conflicts_with_all = ["identity_output", "passphrase"])]
    pub recipient: Vec<String>,
    /// Import one independently named key per recipient from a standard Age list
    #[arg(short = 'R', long, conflicts_with_all = ["identity_output", "passphrase"])]
    pub recipients_file: Vec<PathBuf>,
    /// Create a standard software identity at this private local path
    #[arg(long, conflicts_with = "passphrase")]
    pub identity_output: Option<PathBuf>,
    /// Wrap with a passphrase, acquired using hidden input
    #[arg(long, conflicts_with = "recipient")]
    pub passphrase: bool,
}

#[derive(Subcommand, Debug)]
pub enum UserCommands {
    /// List repository users and their access keys
    List {
        /// Print public user and key metadata as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a repository user's access keys
    Show {
        user: String,
        /// Print public user and key metadata as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a repository user and their first access key
    Add(NewKey),
    /// Revoke a user's keys and rotate encryption for current state
    Revoke {
        user: String,
        #[arg(long, help = "Approve revocation without prompting")]
        yes: bool,
    },
    /// Delete a repository user who has no access keys
    Delete {
        user: String,
        #[arg(long, help = "Approve deletion without prompting")]
        yes: bool,
    },
    /// Manage a repository user's named access keys
    #[command(subcommand)]
    Key(UserKeyCommands),
}

#[derive(Subcommand, Debug)]
pub enum UserKeyCommands {
    /// List a repository user's named access keys
    List {
        user: String,
        /// Print public user and key metadata as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a named access key for an existing repository user
    Add(NewKey),
    /// Detach a key envelope without rotating encryption or revoking learned access
    Detach {
        /// Select a user, or a named key as USER/KEY
        #[arg(value_name = "USER[/KEY]")]
        user: String,
        /// Select the key when USER is given separately
        key: Option<String>,
        #[arg(long, help = "Approve detachment without prompting")]
        yes: bool,
    },
    /// Revoke a key and rotate encryption for current state
    Revoke {
        /// Select a user, or a named key as USER/KEY
        #[arg(value_name = "USER[/KEY]")]
        user: String,
        /// Select the key when USER is given separately
        key: Option<String>,
        #[arg(long, help = "Approve revocation without prompting")]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum KeyCommands {
    /// Rotate the repository encryption key and re-encrypt current state
    Rotate,
}

fn key_reference<'a>(user: &'a str, key: Option<&'a str>) -> Result<(&'a str, &'a str)> {
    let (user, key) = match (user.split_once('/'), key) {
        (Some(reference), None) => reference,
        (None, Some(key)) => (user, key),
        // Reject mixed forms instead of ignoring an extra key on a destructive command.
        _ => anyhow::bail!("specify USER KEY or USER/KEY"),
    };
    access::validate_name(user)?;
    access::validate_name(key)?;
    Ok((user, key))
}

pub fn repository(cli: &super::CLI) -> Result<git2::Repository> {
    if cli.home.is_some() || std::env::var_os("OVER_HOME").is_some() {
        Ok(git2::Repository::discover(cli.resolve_home()?)?)
    } else {
        git2::Repository::discover(".")
            .or_else(|_| git2::Repository::discover(cli.resolve_home().unwrap_or_default()))
            .map_err(Into::into)
    }
}

pub fn policy_inputs(
    repo: &git2::Repository,
    home: &std::path::Path,
    patterns: &[String],
) -> Result<(Vec<String>, std::collections::BTreeMap<PathBuf, PathBuf>)> {
    let cwd = crate::overlays::mapping::physical(&std::env::current_dir()?)?;
    let root = crate::overlays::mapping::physical(access::root(repo)?)?;
    if cwd.starts_with(&root) {
        return Ok((
            patterns
                .iter()
                .map(|pattern| {
                    let absolute = crate::overlays::mapping::absolute(&cwd.join(pattern))?;
                    Ok(crate::overlays::mapping::location(&absolute)?
                        .strip_prefix(&root)?
                        .to_string_lossy()
                        .replace('\\', "/"))
                })
                .collect::<Result<_>>()?,
            Default::default(),
        ));
    }
    let paths = super::common::resolve_inputs_with_missing(patterns, true)?;
    let state = crate::materialize::virtual_checkout::state::state_file()?.inspect_blocking()?;
    let mut mapped = std::collections::BTreeMap::new();
    for physical in &paths {
        let mut candidates = Vec::new();
        let mut projected_namespace = false;
        for (target, record) in &state.checkouts {
            let target = crate::overlays::mapping::physical(
                &crate::materialize::virtual_checkout::state::target_for_key(target)?,
            )?;
            let location = crate::overlays::mapping::location(physical)?;
            let Ok(relative) = location.strip_prefix(&target) else {
                continue;
            };
            projected_namespace |= record.scope.projection.is_some()
                && record.scope.owns_source(relative)
                && !record
                    .scope
                    .target_excludes
                    .iter()
                    .any(|root| relative.starts_with(root));
            if record.scope.owns_target(relative) && record.scope.owns_source(relative) {
                let source = record.source_repository.as_ref().ok_or_else(|| anyhow::anyhow!("legacy virtual association lacks its source; reapply before changing crypt policy"))?;
                ensure!(
                    crate::overlays::mapping::physical(source)? == root,
                    "virtual checkout belongs to another source repository; select its --home explicitly"
                );
                crate::materialize::disposal::validate_relative_ancestors(&target, &location)?;
                candidates.push(record.managed_path.join(relative));
            }
        }
        ensure!(
            candidates.len() <= 1,
            "ambiguous virtual checkout ownership; repair overlapping mappings before changing policy"
        );
        ensure!(
            !candidates.is_empty() || !projected_namespace,
            "unowned virtual destination '{}'; use over add --crypt --overlay NAME PATH to acquire it",
            physical.display()
        );
        if let Some(source) = candidates.pop() {
            mapped.insert(source, physical.clone());
        }
    }
    if !mapped.is_empty() {
        ensure!(
            mapped.len() == paths.len(),
            "selection mixes virtual and unmapped paths; select one canonical scope at a time"
        );
        return Ok((
            mapped
                .keys()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect(),
            mapped,
        ));
    }
    let ctx = crate::exec::Context::builder()
        .repository(crate::overlays::Repository::new(home.to_path_buf()))
        .root(dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home directory unavailable"))?)
        .build();
    let overlay = super::selection::select(&ctx, None, &paths, false)?;
    let ctx = ctx.with_overlay(overlay.clone()).resolve_data()?;
    let routing = overlay.resolve_mappings(&ctx)?;
    let mut inputs = std::collections::BTreeMap::new();
    for physical in paths {
        ensure!(
            physical.is_file(),
            "select individual virtual files for crypt policy"
        );
        let logical = routing.logical_path_for(&physical)?;
        let source = overlay.root.join(logical);
        let relative = crate::overlays::mapping::location(&source)?
            .strip_prefix(&root)?
            .to_path_buf();
        ensure!(
            inputs.insert(relative, physical).is_none(),
            "ambiguous virtual file mapping"
        );
    }
    Ok((
        inputs
            .keys()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect(),
        inputs,
    ))
}

fn policy_repository(cli: &super::CLI, paths: &[String]) -> Result<git2::Repository> {
    if cli.home.is_some() {
        return repository(cli);
    }
    let state = crate::materialize::virtual_checkout::state::state_file()?.inspect_blocking()?;
    let cwd = std::env::current_dir()?;
    let mut sources = std::collections::BTreeSet::new();
    let mut mapped = 0;
    for path in paths {
        let physical = crate::overlays::mapping::location(&cwd.join(path))?;
        let mut owners = Vec::new();
        for (target, record) in &state.checkouts {
            let target = crate::overlays::mapping::physical(
                &crate::materialize::virtual_checkout::state::target_for_key(target)?,
            )?;
            if physical
                .strip_prefix(target)
                .is_ok_and(|relative| record.scope.owns_target(relative))
            {
                owners.push(record);
            }
        }
        ensure!(
            owners.len() <= 1,
            "ambiguous virtual checkout ownership; select and repair its source mapping before changing policy"
        );
        if let Some(record) = owners.first() {
            sources.insert(record.source_repository.clone().ok_or_else(|| {
                anyhow::anyhow!("legacy checkout association has no source; reapply first")
            })?);
            mapped += 1;
        }
    }
    if !sources.is_empty() {
        ensure!(
            sources.len() == 1 && mapped == paths.len(),
            "select paths belonging to one source repository at a time"
        );
        return Ok(git2::Repository::open(
            sources.first().expect("nonempty source set"),
        )?);
    }
    repository(cli)
}

fn can_prompt() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn passphrase(label: &str) -> Result<age::secrecy::SecretString> {
    passphrase_with_confirmation(label, None)
}

fn passphrase_with_confirmation(
    label: &str,
    confirmation: Option<&str>,
) -> Result<age::secrecy::SecretString> {
    if !can_prompt() {
        return Err(crate::crypt::age::InteractionError::Unavailable.into());
    }
    let _pause =
        crate::ui::progress::pause().map_err(|_| crate::crypt::age::InteractionError::Failed)?;
    let theme = crate::ui::style::DialogTheme::default();
    let mut prompt = dialoguer::Password::with_theme(&theme).with_prompt(label);
    if let Some(confirmation) = confirmation {
        prompt = prompt.with_confirmation(confirmation, "Passphrases do not match");
    }
    let entered = zeroize::Zeroizing::new(prompt.interact().map_err(crate::ui::age::prompt_error)?);
    // Avoid shrinking/freeing an ordinary owned String while it still contains the credential.
    Ok(age::secrecy::SecretString::new(entered.as_str().into()))
}

fn add_key(repo: &git2::Repository, opt: &NewKey, new_user: bool) -> Result<()> {
    let theme = crate::ui::style::DialogTheme::default();
    let mut opt = opt.clone();
    // Normalize the compatibility option before applying prompting and import rules.
    opt.name = opt.name.take().or(opt.name_option.take());
    if access::configured(repo)? {
        let registry = access::load(repo)?;
        ensure!(
            registry.users.contains_key(&opt.user) != new_user,
            "user already exists or is missing"
        );
        if access::local_key(repo).is_err() {
            unlock_repository(
                repo,
                &Unlock {
                    identity: Vec::new(),
                    user: None,
                    key: None,
                },
            )?;
        }
    }
    if opt.recipient.is_empty()
        && opt.recipients_file.is_empty()
        && opt.identity_output.is_none()
        && !opt.passphrase
    {
        ensure!(
            can_prompt(),
            "select --recipient, --recipients-file, --identity-output or --passphrase"
        );
        match choisir::Select::new()
            .with_theme(&theme)
            .with_prompt("First key type")
            .items(
                [
                    "Generate Age software identity",
                    "Existing Age/plugin recipient",
                    "Age recipient file",
                    "Passphrase",
                ]
                .into_iter()
                .enumerate(),
            )
            .default(0)
            .interact()?
        {
            0 => {
                opt.identity_output = Some(
                    dialoguer::Input::<String>::with_theme(&theme)
                        .with_prompt("Private identity output path")
                        .interact_text()?
                        .into(),
                )
            }
            1 => opt.recipient.push(
                dialoguer::Input::with_theme(&theme)
                    .with_prompt("Age recipient")
                    .interact_text()?,
            ),
            2 => opt.recipients_file.push(
                dialoguer::Input::<String>::with_theme(&theme)
                    .with_prompt("Age recipient file")
                    .interact_text()?
                    .into(),
            ),
            _ => opt.passphrase = true,
        }
    }
    let mut recipients = opt.recipient.clone();
    for path in &opt.recipients_file {
        recipients.extend(crate::crypt::age::recipients_file(
            path,
            crate::ui::age::Callbacks,
        )?);
    }
    if recipients.len() > 1 && opt.name_prefix.is_none() && opt.name.is_none() && can_prompt() {
        opt.name_prefix = Some(
            dialoguer::Input::with_theme(&theme)
                .with_prompt("Imported key name prefix")
                .interact_text()?,
        );
    }
    ensure!(
        recipients.len() <= 1 || opt.name_prefix.is_some(),
        "supply --name-prefix when importing multiple recipients"
    );
    let name = if let Some(name) = &opt.name {
        name.clone()
    } else if let Some(prefix) = &opt.name_prefix {
        format!("{prefix}-1")
    } else if new_user {
        "default".to_owned()
    } else {
        ensure!(can_prompt(), "supply NAME or --name for an additional key");
        dialoguer::Input::with_theme(&theme)
            .with_prompt("Key name")
            .interact_text()?
    };
    let mut recipient = recipients.first().cloned();
    let mut password = None;
    let mut generated = None;
    if let Some(path) = &opt.identity_output {
        ensure!(!path.exists(), "identity output already exists");
        let absolute = crate::overlays::mapping::absolute(path)?;
        let parent = absolute
            .parent()
            .ok_or_else(|| anyhow::anyhow!("identity output has no parent"))?
            .canonicalize()?;
        let physical = parent.join(
            absolute
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("identity output needs a filename"))?,
        );
        match git2::Repository::discover(&parent) {
            Ok(containing) => {
                if let Some(workdir) = containing.workdir() {
                    // A private identity must never become an ordinary tracked plaintext file.
                    ensure!(
                        !physical.starts_with(workdir.canonicalize()?)
                            || physical.starts_with(containing.commondir().canonicalize()?),
                        "write private Age identities outside Git working trees or inside private Git administrative storage"
                    );
                }
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let identity = age::x25519::Identity::generate();
        recipient = Some(identity.to_public().to_string());
        generated = Some((path, identity));
    } else if opt.passphrase {
        // Catch enrollment typos before granting access with an unusable passphrase.
        password = Some(passphrase_with_confirmation(
            &format!("New Age key passphrase for {}/{name}", opt.user),
            Some(&format!(
                "Confirm new Age key passphrase for {}/{name}",
                opt.user
            )),
        )?);
    }
    // Publish a generated identity before granting access; a failure leaves recovery material.
    if let Some((path, identity)) = &generated {
        use age::secrecy::ExposeSecret;
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        #[cfg(windows)]
        access::windows_acl(path, true)?;
        writeln!(file, "{}", identity.to_string().expose_secret())?;
        file.sync_all()?;
    }
    let entries: Vec<_> = if recipients.len() > 1 {
        recipients
            .into_iter()
            .enumerate()
            .map(|(index, recipient)| {
                (
                    format!(
                        "{}-{}",
                        opt.name_prefix.as_deref().expect("validated prefix"),
                        index + 1
                    ),
                    Some(recipient),
                    None,
                )
            })
            .collect()
    } else {
        vec![(name, recipient, password)]
    };
    let names: Vec<_> = entries.iter().map(|(name, _, _)| name.clone()).collect();
    access::add_keys_committed(
        repo,
        &opt.user,
        entries,
        new_user,
        crate::ui::age::Callbacks,
    )?;
    if names.len() > 1 {
        for name in names {
            eprintln!("Added key {}/{name}", opt.user);
        }
    }
    if let Some((path, identity)) = generated
        && access::remember_identity(repo, &identity.to_public().to_string(), path).is_err()
    {
        // Optional hint persistence happens after the complete user/key has been published.
        eprintln!(
            "User/key created, but its local identity hint could not be saved; supply --identity when unlocking."
        );
    }
    Ok(())
}

pub fn user(cli: &super::CLI, command: &UserCommands) -> Result<()> {
    let repo = repository(cli)?;
    match command {
        UserCommands::Add(opt) => add_key(&repo, opt, true),
        UserCommands::Delete { user, yes } => {
            super::common::confirm_removal(
                &format!("Delete empty user {user:?} from {}?", repo.path().display()),
                *yes,
                false,
            )?;
            access::delete_user_committed(&repo, user)
        }
        UserCommands::Revoke { user, yes } => {
            super::common::confirm_removal(
                &format!(
                    "Revoke user {user:?} in {} and rotate/re-encrypt current state (history remains accessible)?",
                    repo.path().display()
                ),
                *yes,
                false,
            )?;
            policy::rotate_committed_with(
                &repo,
                Some((user, None)),
                passphrase,
                crate::ui::age::Callbacks,
            )
        }
        UserCommands::Key(UserKeyCommands::Add(opt)) => add_key(&repo, opt, false),
        UserCommands::Key(UserKeyCommands::Detach { user, key, yes }) => {
            let (user, key) = key_reference(user, key.as_deref())?;
            eprintln!(
                "Detaching only removes the current envelope; it does not revoke a learned key or historical access."
            );
            super::common::confirm_removal(
                &format!(
                    "Detach key {key:?} for user {user:?} in {}?",
                    repo.path().display()
                ),
                *yes,
                false,
            )?;
            access::detach_committed(&repo, user, key)
        }
        UserCommands::Key(UserKeyCommands::Revoke { user, key, yes }) => {
            let (user, key) = key_reference(user, key.as_deref())?;
            super::common::confirm_removal(
                &format!(
                    "Revoke key {key:?} for user {user:?} in {} and rotate/re-encrypt current state (history remains accessible)?",
                    repo.path().display()
                ),
                *yes,
                false,
            )?;
            policy::rotate_committed_with(
                &repo,
                Some((user, Some(key))),
                passphrase,
                crate::ui::age::Callbacks,
            )
        }
        command => {
            let registry = public_registry(&repo)?;
            let (selected_user, json) = match command {
                UserCommands::List { json } => (None, *json),
                UserCommands::Show { user, json }
                | UserCommands::Key(UserKeyCommands::List { user, json }) => (Some(user), *json),
                _ => unreachable!("mutating commands are handled above"),
            };
            if let Some(user) = selected_user {
                ensure!(registry.users.contains_key(user), "unknown repository user");
            }
            if json {
                return crate::ui::json::print_json(&public_users(&registry, selected_user));
            }
            for (name, keys) in registry.users {
                let selected = selected_user.is_none_or(|user| name == *user);
                if selected {
                    println!("{name}");
                    if !matches!(command, UserCommands::List { .. }) {
                        for (name, key) in keys {
                            println!(
                                "  {name}: {}",
                                key.recipient.as_deref().unwrap_or("Age passphrase")
                            );
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

fn public_registry(repo: &git2::Repository) -> Result<access::Registry> {
    if !repo.is_bare() {
        return access::load(repo);
    }
    // Bare administrative files are not a worktree: only the captured HEAD tree is authoritative.
    let tree = repo.head()?.peel_to_tree()?;
    access::registry_from_tree(repo, &tree)?.ok_or_else(|| {
        anyhow::anyhow!("repository access metadata is missing from the HEAD commit")
    })
}

#[derive(serde::Serialize)]
struct PublicUsers<'a> {
    users: Vec<PublicUser<'a>>,
}

#[derive(serde::Serialize)]
struct PublicUser<'a> {
    name: &'a str,
    keys: Vec<PublicKey<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum PublicKeyKind {
    Passphrase,
    Recipient,
}

#[derive(serde::Serialize)]
struct PublicKey<'a> {
    name: &'a str,
    kind: PublicKeyKind,
    recipient: Option<&'a str>,
}

fn public_users<'a>(registry: &'a access::Registry, selected: Option<&String>) -> PublicUsers<'a> {
    // A dedicated projection keeps envelopes and future private registry fields out of the CLI API.
    PublicUsers {
        users: registry
            .users
            .iter()
            .filter(|(name, _)| selected.is_none_or(|user| user == *name))
            .map(|(name, keys)| PublicUser {
                name,
                keys: keys
                    .iter()
                    .map(|(name, key)| PublicKey {
                        name,
                        kind: if key.recipient.is_some() {
                            PublicKeyKind::Recipient
                        } else {
                            PublicKeyKind::Passphrase
                        },
                        recipient: key.recipient.as_deref(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn key(cli: &super::CLI, _command: &KeyCommands) -> Result<()> {
    policy::rotate_committed_with(
        &repository(cli)?,
        None,
        passphrase,
        crate::ui::age::Callbacks,
    )
}

pub fn unlock(cli: &super::CLI, opt: &Unlock) -> Result<()> {
    let repo = repository(cli)?;
    unlock_repository(&repo, opt)
}

pub fn unlock_repository(repo: &git2::Repository, opt: &Unlock) -> Result<()> {
    // Only unlock materialization journals may be resumed instead of explicit recovery.
    let uses_local_key = policy::unlock_uses_local_key(repo)?;
    if opt.identity.is_empty() && opt.user.is_none() && opt.key.is_none() && uses_local_key {
        // Key presence avoids another credential prompt, but cannot prove filter/checkout health.
        return policy::refresh(repo, false);
    }
    let registry = access::load(repo)?;
    let mut identity = None;
    policy::unlock_with(repo, |repo| {
        let credentials = acquire_repository_key(repo, &registry, opt)?;
        identity = credentials.identity;
        Ok(credentials.key)
    })?;
    remember_acquired_identity(repo, identity.as_ref());
    Ok(())
}

pub(crate) struct AcquiredKey {
    pub key: crate::crypt::format::RepositoryKey,
    identity: Option<(String, PathBuf)>,
}

impl AcquiredKey {
    pub(crate) fn unlock(self, repo: &git2::Repository) -> Result<()> {
        policy::unlock_with(repo, |_| Ok(self.key))?;
        remember_acquired_identity(repo, self.identity.as_ref());
        Ok(())
    }

    pub(crate) fn remember(&self, repo: &git2::Repository) {
        remember_acquired_identity(repo, self.identity.as_ref());
    }
}

fn remember_acquired_identity(repo: &git2::Repository, identity: Option<&(String, PathBuf)>) {
    if let Some((recipient, path)) = identity {
        // Credential convenience metadata cannot turn a completed unlock into failure.
        if access::remember_identity(repo, recipient, path).is_err() {
            eprintln!(
                "Repository unlocked, but the local identity hint could not be saved; supply --identity next time."
            );
        }
    }
}

pub(crate) fn acquire_repository_key(
    repo: &git2::Repository,
    registry: &access::Registry,
    opt: &Unlock,
) -> Result<AcquiredKey> {
    acquire_repository_key_policy(repo, registry, opt, true)
}

pub(crate) fn acquire_repository_key_policy(
    repo: &git2::Repository,
    registry: &access::Registry,
    opt: &Unlock,
    allow_temporary_files: bool,
) -> Result<AcquiredKey> {
    let mut candidates: Vec<_> = registry
        .users
        .iter()
        .filter(|(user, _)| opt.user.as_ref().is_none_or(|selected| selected == *user))
        .flat_map(|(user, keys)| {
            keys.iter()
                .filter(|(key, _)| opt.key.as_ref().is_none_or(|selected| selected == *key))
                .map(move |(key, access)| (user, key, access))
        })
        .collect();
    ensure!(!candidates.is_empty(), "no matching repository key");
    let selection = if candidates.len() > 1 && opt.identity.is_empty() && can_prompt() {
        let labels: Vec<_> = candidates
            .iter()
            .map(|(user, name, key)| {
                let kind = if key.recipient.is_some() {
                    "Age recipient"
                } else {
                    "Age passphrase"
                };
                format!("{user}/{name} ({kind})")
            })
            .collect();
        let _pause = crate::ui::progress::pause()?;
        let index = choisir::Select::new()
            .with_theme(&crate::ui::style::DialogTheme::default())
            .search(choisir::SearchMode::Typeahead)
            .with_prompt("Choose a repository key")
            .items(labels.iter().enumerate())
            .default(0)
            .interact_on_opt(&console::Term::stderr())
            .map_err(crate::ui::age::choice_error)?
            .ok_or(crate::crypt::age::InteractionError::Cancelled)?;
        let selected = candidates[index];
        // Narrow before hints or identity loading so an unselected device cannot interact or recover.
        candidates = vec![selected];
        Some((selected.0.as_str(), selected.1.as_str()))
    } else {
        None
    };
    let mut sources = opt.identity.clone();
    for (_, _, key) in candidates.iter().filter(|_| opt.identity.is_empty()) {
        if let Some(recipient) = &key.recipient {
            match access::identity_hint(repo, recipient) {
                Ok(Some(path)) if !sources.contains(&path) => sources.push(path),
                Ok(_) => (),
                // Local hints are conveniences, not a reason to suppress recovery credentials.
                Err(_) => {
                    eprintln!("A local identity hint is unavailable; continuing without that hint.")
                }
            }
        }
    }
    if sources.is_empty()
        && candidates.iter().any(|(_, _, key)| key.recipient.is_some())
        && can_prompt()
    {
        let prompt = if candidates.iter().any(|(_, _, key)| key.recipient.is_none()) {
            "Age identity file (leave empty to use passphrase keys)"
        } else {
            "Age identity file"
        };
        let _pause = crate::ui::progress::pause()?;
        let path: String = dialoguer::Input::with_theme(&crate::ui::style::DialogTheme::default())
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text_on(&console::Term::stderr())
            .map_err(crate::ui::age::prompt_error)?;
        if !path.is_empty() {
            sources.push(path.into());
        }
    }
    let mut used_identity = None;
    let key = access::unwrap_registry_policy(
        registry,
        access::UnwrapOptions {
            sources: &sources,
            user: selection.map(|(user, _)| user).or(opt.user.as_deref()),
            name: selection.map(|(_, name)| name).or(opt.key.as_deref()),
            allow_temporary_files,
        },
        crate::ui::age::Callbacks,
        passphrase,
        |recipient, path| used_identity = Some((recipient.to_owned(), path.to_path_buf())),
    )?;
    Ok(AcquiredKey {
        key,
        identity: used_identity,
    })
}

pub fn lock(cli: &super::CLI) -> Result<()> {
    policy::refresh(&repository(cli)?, true)
}

pub fn execute(cli: &super::CLI, command: &Commands) -> Result<()> {
    match command {
        // Git invokes helpers in its own repository context, independently of Over home.
        Commands::Filter(FilterCommand::Clean { path }) => {
            crate::crypt::filter::once_path(true, path.as_deref())
        }
        Commands::Filter(FilterCommand::Smudge { path }) => {
            crate::crypt::filter::once_path(false, path.as_deref())
        }
        Commands::FilterProcess => crate::crypt::filter::process(),
        Commands::Textconv { file } => crate::crypt::textconv::run(file),
        Commands::Diff { args } => crate::crypt::diff::run(args),
        Commands::Add { paths } => {
            let repo = policy_repository(cli, paths)?;
            let (patterns, inputs) = policy_inputs(&repo, &cli.resolve_home()?, paths)?;
            policy::update_inputs(&repo, &patterns, true, &inputs)
        }
        Commands::Remove { paths, yes } => {
            let repo = policy_repository(cli, paths)?;
            eprintln!(
                "Removing crypt protection stages plaintext for future commits. Historical ciphertext/plaintext is unchanged."
            );
            super::common::confirm_removal(
                &format!(
                    "Stage plaintext and remove protection for {paths:?} in {}?",
                    repo.path().display()
                ),
                *yes,
                false,
            )?;
            let (patterns, inputs) = policy_inputs(&repo, &cli.resolve_home()?, paths)?;
            policy::update_inputs(&repo, &patterns, false, &inputs)
        }
        Commands::List => {
            for finding in policy::inspect(&repository(cli)?)? {
                crate::ui::info(finding.to_string())?;
            }
            Ok(())
        }
        Commands::Recover => policy::recover(&repository(cli)?),
        Commands::Migrate => policy::migrate(&repository(cli)?),
    }
}
