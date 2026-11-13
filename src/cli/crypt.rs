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
}

#[derive(Args, Debug)]
pub struct Unlock {
    /// Read a standard Age identity file
    #[arg(long)]
    pub identity: Option<PathBuf>,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub key: Option<String>,
}

#[derive(Args, Debug)]
pub struct NewKey {
    pub user: String,
    #[arg(long)]
    pub name: Option<String>,
    /// Wrap with an existing standard Age public recipient
    #[arg(long, conflicts_with = "identity_output")]
    pub recipient: Option<String>,
    /// Create a standard software identity at this private local path
    #[arg(long, conflicts_with = "passphrase")]
    pub identity_output: Option<PathBuf>,
    /// Wrap with a passphrase, acquired using hidden input
    #[arg(long, conflicts_with = "recipient")]
    pub passphrase: bool,
}

#[derive(Subcommand, Debug)]
pub enum UserCommands {
    List,
    Show {
        user: String,
    },
    Add(NewKey),
    Revoke {
        user: String,
    },
    Delete {
        user: String,
    },
    #[command(subcommand)]
    Key(UserKeyCommands),
}

#[derive(Subcommand, Debug)]
pub enum UserKeyCommands {
    List { user: String },
    Add(NewKey),
    Detach { user: String, key: String },
    Revoke { user: String, key: String },
}

#[derive(Subcommand, Debug)]
pub enum KeyCommands {
    Rotate,
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
    let overlay = crate::selection::select(&ctx, None, &paths, false)?;
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
    ensure!(
        can_prompt(),
        "unlock requires a terminal credential prompt; use --identity for noninteractive access"
    );
    let _pause = crate::ui::progress::pause()?;
    let entered = zeroize::Zeroizing::new(
        dialoguer::Password::new()
            .with_prompt(label)
            .interact()
            .map_err(|_| anyhow::anyhow!("secure credential input unavailable"))?,
    );
    // Avoid shrinking/freeing an ordinary owned String while it still contains the credential.
    Ok(age::secrecy::SecretString::new(entered.as_str().into()))
}

fn add_key(repo: &git2::Repository, opt: &NewKey, new_user: bool) -> Result<()> {
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
                    identity: None,
                    user: None,
                    key: None,
                },
            )?;
        }
    }
    let name = if let Some(name) = &opt.name {
        name.clone()
    } else if new_user {
        "default".to_owned()
    } else {
        ensure!(can_prompt(), "supply --name for an additional key");
        dialoguer::Input::new()
            .with_prompt("Key name")
            .interact_text()?
    };
    let mut recipient = opt.recipient.clone();
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
        password = Some(passphrase("New Age key passphrase")?);
    } else if recipient.is_none() {
        ensure!(
            can_prompt(),
            "select --recipient, --identity-output or --passphrase"
        );
        let selection = dialoguer::Select::new()
            .with_prompt("First key type")
            .items(["Age software recipient", "Passphrase"])
            .default(0)
            .interact()?;
        if selection == 0 {
            recipient = Some(
                dialoguer::Input::new()
                    .with_prompt("Age recipient")
                    .interact_text()?,
            );
        } else {
            password = Some(passphrase("New Age key passphrase")?);
        }
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
    access::add_key(
        repo,
        &opt.user,
        &name,
        recipient.as_deref(),
        password,
        new_user,
    )?;
    if let Some((path, identity)) = generated {
        access::remember_identity(repo, &identity.to_public().to_string(), path)?;
    }
    Ok(())
}

pub fn user(cli: &super::CLI, command: &UserCommands) -> Result<()> {
    let repo = repository(cli)?;
    match command {
        UserCommands::Add(opt) => add_key(&repo, opt, true),
        UserCommands::Delete { user } => access::delete_user(&repo, user),
        UserCommands::Revoke { user } => policy::rotate(&repo, Some((user, None)), passphrase),
        UserCommands::Key(UserKeyCommands::Add(opt)) => add_key(&repo, opt, false),
        UserCommands::Key(UserKeyCommands::Detach { user, key }) => {
            eprintln!(
                "Detaching only removes the current envelope; it does not revoke a learned key or historical access."
            );
            access::detach(&repo, user, key)
        }
        UserCommands::Key(UserKeyCommands::Revoke { user, key }) => {
            policy::rotate(&repo, Some((user, Some(key))), passphrase)
        }
        command => {
            let registry = access::load(&repo)?;
            if let UserCommands::Show { user } | UserCommands::Key(UserKeyCommands::List { user }) =
                command
            {
                ensure!(registry.users.contains_key(user), "unknown repository user");
            }
            for (name, keys) in registry.users {
                let selected = match command {
                    UserCommands::Show { user }
                    | UserCommands::Key(UserKeyCommands::List { user }) => name == *user,
                    _ => true,
                };
                if selected {
                    println!("{name}");
                    if !matches!(command, UserCommands::List) {
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

pub fn key(cli: &super::CLI, _command: &KeyCommands) -> Result<()> {
    policy::rotate(&repository(cli)?, None, passphrase)
}

pub fn unlock(cli: &super::CLI, opt: &Unlock) -> Result<()> {
    let repo = repository(cli)?;
    unlock_repository(&repo, opt)
}

pub fn unlock_repository(repo: &git2::Repository, opt: &Unlock) -> Result<()> {
    // Interrupted operations and unsafe storage cannot be repaired by another password prompt.
    {
        let _inspection = access::guard(repo, false)?;
    }
    if opt.identity.is_none()
        && opt.user.is_none()
        && opt.key.is_none()
        && access::local_key(repo).is_ok()
    {
        // Key presence avoids another credential prompt, but cannot prove filter/checkout health.
        return policy::refresh(repo, false);
    }
    let registry = access::load(repo)?;
    let candidates: Vec<_> = registry
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
    if let Some(identity) = &opt.identity {
        access::unlock(
            repo,
            Some(identity),
            None,
            opt.user.as_deref(),
            opt.key.as_deref(),
        )?;
    } else {
        // Locally generated standard identities can be reused without re-entering a path.
        let known = candidates
            .iter()
            .find_map(|(_, _, key)| {
                key.recipient
                    .as_ref()
                    .and_then(|recipient| access::identity_hint(repo, recipient).transpose())
            })
            .transpose()?;
        if let Some(identity) = known {
            access::unlock(
                repo,
                Some(&identity),
                None,
                opt.user.as_deref(),
                opt.key.as_deref(),
            )?;
        } else {
            let selected = if candidates.len() == 1 {
                0
            } else {
                ensure!(
                    can_prompt(),
                    "choose --identity or select --user/--key before interactive unlock"
                );
                let labels: Vec<_> = candidates
                    .iter()
                    .map(|(user, key, _)| format!("{user}/{key}"))
                    .collect();
                dialoguer::Select::with_theme(&crate::ui::style::DialogTheme::default())
                    .with_prompt("Unlock with key")
                    .items(&labels)
                    .default(0)
                    .interact()?
            };
            let (user, name, key) = candidates[selected];
            if key.recipient.is_some() {
                ensure!(
                    can_prompt(),
                    "supply --identity <standard-age-identity-file>"
                );
                let identity: String =
                    dialoguer::Input::with_theme(&crate::ui::style::DialogTheme::default())
                        .with_prompt("Age identity file")
                        .interact_text()?;
                access::unlock(
                    repo,
                    Some(std::path::Path::new(&identity)),
                    None,
                    Some(user),
                    Some(name),
                )?;
            } else {
                access::unlock(
                    repo,
                    None,
                    Some(passphrase("Age key passphrase")?),
                    Some(user),
                    Some(name),
                )?;
            }
        }
    }
    Ok(())
}

pub fn lock(cli: &super::CLI) -> Result<()> {
    policy::refresh(&repository(cli)?, true)
}

pub fn execute(cli: &super::CLI, command: &Commands) -> Result<()> {
    let repo = match command {
        Commands::Add { paths } | Commands::Remove { paths, .. } => policy_repository(cli, paths)?,
        _ => repository(cli)?,
    };
    match command {
        Commands::Add { paths } => {
            let (patterns, inputs) = policy_inputs(&repo, &cli.resolve_home()?, paths)?;
            policy::update_inputs(&repo, &patterns, true, &inputs)
        }
        Commands::Remove { paths, yes } => {
            eprintln!(
                "Removing crypt protection stages plaintext for future commits. Historical ciphertext/plaintext is unchanged."
            );
            ensure!(
                *yes || (can_prompt()
                    && dialoguer::Confirm::new()
                        .with_prompt("Stage plaintext and remove protection?")
                        .default(false)
                        .interact()?),
                "confirmation required; use --yes deliberately"
            );
            let (patterns, inputs) = policy_inputs(&repo, &cli.resolve_home()?, paths)?;
            policy::update_inputs(&repo, &patterns, false, &inputs)
        }
        Commands::List => {
            for finding in policy::inspect(&repo)? {
                crate::ui::info(finding.to_string())?;
            }
            Ok(())
        }
        Commands::Recover => policy::recover(&repo),
        Commands::Migrate => policy::migrate(&repo),
    }
}
