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
    #[arg(long, conflicts_with = "name_prefix")]
    pub name: Option<String>,
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
    let mut opt = opt.clone();
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
        match dialoguer::Select::new()
            .with_prompt("First key type")
            .items([
                "Generate Age software identity",
                "Existing Age/plugin recipient",
                "Age recipient file",
                "Passphrase",
            ])
            .default(0)
            .interact()?
        {
            0 => {
                opt.identity_output = Some(
                    dialoguer::Input::<String>::new()
                        .with_prompt("Private identity output path")
                        .interact_text()?
                        .into(),
                )
            }
            1 => opt.recipient.push(
                dialoguer::Input::new()
                    .with_prompt("Age recipient")
                    .interact_text()?,
            ),
            2 => opt.recipients_file.push(
                dialoguer::Input::<String>::new()
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
            dialoguer::Input::new()
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
        ensure!(can_prompt(), "supply --name for an additional key");
        dialoguer::Input::new()
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
        password = Some(passphrase("New Age key passphrase")?);
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
    access::add_keys(
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
        UserCommands::Delete { user } => access::delete_user(&repo, user),
        UserCommands::Revoke { user } => policy::rotate_with(
            &repo,
            Some((user, None)),
            passphrase,
            crate::ui::age::Callbacks,
        ),
        UserCommands::Key(UserKeyCommands::Add(opt)) => add_key(&repo, opt, false),
        UserCommands::Key(UserKeyCommands::Detach { user, key }) => {
            eprintln!(
                "Detaching only removes the current envelope; it does not revoke a learned key or historical access."
            );
            access::detach(&repo, user, key)
        }
        UserCommands::Key(UserKeyCommands::Revoke { user, key }) => policy::rotate_with(
            &repo,
            Some((user, Some(key))),
            passphrase,
            crate::ui::age::Callbacks,
        ),
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
    policy::rotate_with(
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
    // Interrupted operations and unsafe storage cannot be repaired by another password prompt.
    {
        let _inspection = access::lifecycle_guard(repo, false)?;
    }
    if opt.identity.is_empty()
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
    let mut sources = opt.identity.clone();
    for (_, _, key) in candidates.iter().filter(|_| opt.identity.is_empty()) {
        if let Some(recipient) = &key.recipient {
            match access::identity_hint(repo, recipient) {
                Ok(Some(path)) if !sources.contains(&path) => sources.push(path),
                Ok(_) => (),
                // Local hints are conveniences, not a reason to suppress recovery credentials.
                Err(_) => {
                    eprintln!("A local identity hint is unavailable; continuing with other keys.")
                }
            }
        }
    }
    if sources.is_empty()
        && candidates.iter().any(|(_, _, key)| key.recipient.is_some())
        && can_prompt()
    {
        let path: String = dialoguer::Input::new()
            .with_prompt("Age identity file (leave empty to use passphrase keys)")
            .allow_empty(true)
            .interact_text()?;
        if !path.is_empty() {
            sources.push(path.into());
        }
    }
    let mut used_identity = None;
    policy::unlock_with(repo, |repo| {
        access::unwrap_sources_with(
            repo,
            &sources,
            opt.user.as_deref(),
            opt.key.as_deref(),
            crate::ui::age::Callbacks,
            passphrase,
            |recipient, path| used_identity = Some((recipient.to_owned(), path.to_path_buf())),
        )
    })?;
    if let Some((recipient, path)) = used_identity {
        // Credential convenience metadata must not turn a completed unlock into an apparent failure.
        if access::remember_identity(repo, &recipient, &path).is_err() {
            eprintln!(
                "Repository unlocked, but the local identity hint could not be saved; supply --identity next time."
            );
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
