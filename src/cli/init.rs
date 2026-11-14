use std::io::{self, IsTerminal};

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use dialoguer::{Confirm, Select};

use crate::actions::git::init::prepare;
use crate::overlays::MaterializationKind;
use crate::ui::{self, style::DialogTheme};

use super::CLI;

#[derive(Args, Debug)]
pub struct Params {
    /// Clone from a Git URL or local repository path
    source: Option<String>,

    /// Create a missing root configuration with this repository default
    #[arg(long, value_enum, conflicts_with = "no_config")]
    mode: Option<SetupMode>,
    /// Continue without creating missing root configuration
    #[arg(long)]
    no_config: bool,

    /// Set an existing checkout's canonical origin without prompting
    #[arg(long)]
    set_origin: bool,
    /// Explicitly unlock repository encryption during initialization
    #[arg(long)]
    unlock: bool,
    /// Use a standard Age identity when unlocking
    #[arg(short = 'i', long, requires = "unlock")]
    identity: Vec<std::path::PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SetupMode {
    Checkout,
    Symlink,
}

impl SetupMode {
    fn materialization(self) -> MaterializationKind {
        match self {
            Self::Checkout => MaterializationKind::Checkout,
            Self::Symlink => MaterializationKind::Symlink,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Checkout => "virtual checkout (mode: checkout)",
            Self::Symlink => "symlink (mode: symlink)",
        }
    }
}

fn choose_configuration(
    mode: Option<SetupMode>,
    no_config: bool,
    interactive: bool,
    confirm: impl FnOnce() -> Result<bool>,
    select: impl FnOnce() -> Result<SetupMode>,
) -> Result<Option<SetupMode>> {
    if mode.is_some() || no_config {
        return Ok(mode);
    }
    if !interactive {
        bail!(
            "no root Over configuration found; rerun over init --mode checkout (recommended), --mode symlink, or --no-config to skip creation"
        );
    }
    if confirm()? {
        Ok(Some(select()?))
    } else {
        Ok(None)
    }
}

fn setup_configuration(
    result: &crate::actions::git::init::Initialization,
    mode: Option<SetupMode>,
    no_config: bool,
) -> Result<()> {
    if let Some(path) = result.repository.root_descriptor()? {
        if mode.is_some() {
            ui::info(format!(
                "Existing configuration {} preserved; --mode only creates missing configuration. Existing repository and overlay policy remains authoritative.",
                path.display()
            ))?;
        }
        return Ok(());
    }
    let selected = choose_configuration(
        mode,
        no_config,
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        || {
            let _pause = crate::ui::progress::pause()?;
            Confirm::with_theme(&DialogTheme::default())
                .with_prompt("No root Over configuration found. Create over.toml?")
                .default(true)
                .interact()
                .context("configuration creation confirmation failed")
        },
        || {
            let _pause = crate::ui::progress::pause()?;
            ui::info(
                "Virtual checkout uses ordinary target files from committed source HEAD; symlink edits target files through links to the source.",
            )?;
            let selection = Select::with_theme(&DialogTheme::default())
                .with_prompt(
                    "Choose the repository default materialization (no files will be applied)",
                )
                .items(["Virtual checkout (recommended)", "Symlink"])
                .default(0)
                .interact()
                .context("materialization selection failed")?;
            Ok(if selection == 0 {
                SetupMode::Checkout
            } else {
                SetupMode::Symlink
            })
        },
    )?;
    if let Some(mode) = selected {
        let path = result.create_configuration(mode.materialization())?;
        ui::info(format!(
            "Created {} with repository default {}; nearer overlay and rule policy still takes precedence. No deployed files were changed.",
            path.display(),
            mode.description()
        ))?;
    } else {
        ui::info(
            "Root configuration creation skipped. Implicit materialization remains symlink unless child policy overrides it; only discovered child overlays are available. Create configuration later with over init --mode checkout or --mode symlink.",
        )?;
    }
    Ok(())
}

pub async fn execute(cli: &CLI, params: &Params) -> Result<()> {
    tracing::debug!("init command");
    let destination = cli.resolve_home()?;
    let source = params.source.clone();
    let mode = params.mode;
    let no_config = params.no_config;
    let mut result = tokio::task::spawn_blocking(move || {
        let result = prepare(destination, source.as_deref())?;
        setup_configuration(&result, mode, no_config)?;
        Ok::<_, anyhow::Error>(result)
    })
    .await
    .context("initialization task failed")??;
    if result.needs_origin_update() {
        confirm_origin_change(
            params.set_origin,
            io::stdin().is_terminal() && io::stderr().is_terminal(),
            || {
                let _pause = crate::ui::progress::pause()?;
                let prompt = format!(
                    "Set origin from {} to {}?",
                    crate::utils::short_path(result.origin.as_deref().unwrap_or("<missing>")),
                    crate::utils::short_path(result.canonical.as_deref().unwrap_or("<missing>"))
                );
                Confirm::with_theme(&DialogTheme::default())
                    .with_prompt(prompt)
                    .default(false)
                    .interact()
                    .context("origin confirmation failed; use --set-origin for non-interactive initialization")
            },
        )?;
        result = tokio::task::spawn_blocking(move || {
            result.set_origin()?;
            Ok::<_, anyhow::Error>(result)
        })
        .await
        .context("origin update task failed")??;
    }
    let git = git2::Repository::open(&result.destination)?;
    crate::crypt::access::configure_filters(&git)?;
    if crate::crypt::access::configured(&git)? && crate::crypt::access::local_key(&git).is_ok() {
        // A persisted key survives restart; repair any incomplete ciphertext refresh without prompting.
        crate::crypt::policy::refresh(&git, false)?;
    } else if crate::crypt::access::configured(&git)? {
        let consent = params.unlock
            || (io::stdin().is_terminal()
                && io::stderr().is_terminal()
                && Confirm::new()
                    .with_prompt("Unlock repository encryption before bootstrap hooks?")
                    .default(false)
                    .interact()?);
        if consent {
            super::crypt::unlock_repository(
                &git,
                &super::crypt::Unlock {
                    identity: params.identity.clone(),
                    user: None,
                    key: None,
                },
            )?;
        }
    }
    // Do not discover child overlays until bootstrap tools have unlocked them.
    let repository = result.repository.clone();
    let hooks = repository.hooks()?;
    if crate::crypt::access::configured(&git)?
        && (!hooks.post_init.is_empty() || (result.cloned && !hooks.post_clone.is_empty()))
    {
        // Shell hooks cannot declare all file reads; never start them against a locked ciphertext checkout.
        crate::crypt::access::local_key(&git).context("bootstrap hooks were not run because repository encryption is locked; run over unlock, then retry over init (existing checkouts run post-init)")?;
    }
    // Bootstrap must not discover encrypted children before unlock hooks execute.
    let templates = crate::exec::Context::builder()
        .repository(repository.clone())
        .build();
    let events: &[crate::hooks::Event] = if result.cloned {
        &[
            crate::hooks::Event::PostClone,
            crate::hooks::Event::PostInit,
        ]
    } else {
        &[crate::hooks::Event::PostInit]
    };
    for event in events {
        crate::hooks::run(
            &hooks,
            *event,
            crate::hooks::HookContext {
                templates: templates.clone(),
                repository: &repository.root,
                cwd: &repository.root,
                scope: "repository root",
                overlay: None,
                target_root: None,
            },
        )
        .await?;
    }
    ui::info(format!(
        "{} Over repository at {}",
        if result.cloned {
            "Cloned"
        } else {
            "Validated existing"
        },
        result.destination.display()
    ))?;
    if params.source.is_some() && !result.cloned {
        ui::info("Existing checkout reused; source was not cloned or fetched")?;
    }
    ui::info(format!(
        "origin: {}",
        result.origin.as_deref().unwrap_or("<not configured>")
    ))?;
    if let Some(push_url) = result.push_url {
        ui::info(format!("Explicit origin push URL preserved: {push_url}"))?;
    }
    ui::info("Next: over apply [NAME]")?;
    Ok(())
}

fn confirm_origin_change(
    set_origin: bool,
    interactive: bool,
    prompt: impl FnOnce() -> Result<bool>,
) -> Result<()> {
    if set_origin {
        return Ok(());
    }
    if !interactive {
        bail!(
            "origin differs from the configured repository; rerun over init --set-origin to approve the change"
        );
    }
    if !prompt()? {
        bail!("initialization incomplete: origin change declined; origin preserved");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(Some(SetupMode::Checkout), false)]
    #[case(Some(SetupMode::Symlink), false)]
    #[case(None, true)]
    fn explicit_configuration_choices_never_prompt(
        #[case] mode: Option<SetupMode>,
        #[case] no_config: bool,
    ) {
        assert_eq!(
            choose_configuration(
                mode,
                no_config,
                false,
                || panic!("must not prompt"),
                || { panic!("must not prompt") }
            )
            .unwrap(),
            mode
        );
    }

    #[rstest::rstest]
    #[case(SetupMode::Checkout)]
    #[case(SetupMode::Symlink)]
    fn accepting_configuration_creation_uses_the_selected_mode(#[case] mode: SetupMode) {
        assert_eq!(
            choose_configuration(None, false, true, || Ok(true), || Ok(mode)).unwrap(),
            Some(mode)
        );
    }

    #[test]
    fn declining_configuration_creation_never_asks_for_a_mode() {
        assert_eq!(
            choose_configuration(
                None,
                false,
                true,
                || Ok(false),
                || panic!("must not select")
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn noninteractive_missing_configuration_requires_an_explicit_decision() {
        let error = choose_configuration(
            None,
            false,
            false,
            || panic!("must not prompt"),
            || panic!("must not select"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--mode checkout"));
        assert!(error.to_string().contains("--no-config"));
    }

    #[test]
    fn cancelled_mode_selection_propagates_the_error() {
        assert!(
            choose_configuration(None, false, true, || Ok(true), || bail!("cancelled"))
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    }

    #[test]
    fn explicit_origin_approval_never_prompts_even_without_a_terminal() {
        confirm_origin_change(true, false, || panic!("must not prompt")).unwrap();
    }

    #[test]
    fn noninteractive_confirmation_explains_how_to_approve() {
        let error = confirm_origin_change(false, false, || panic!("must not prompt")).unwrap_err();
        assert!(error.to_string().contains("--set-origin"));
    }

    #[rstest::rstest]
    #[case(true)]
    #[case(false)]
    fn interactive_confirmation_respects_the_users_answer(#[case] approved: bool) {
        let result = confirm_origin_change(false, true, || Ok(approved));
        assert_eq!(result.is_ok(), approved);
    }
}
