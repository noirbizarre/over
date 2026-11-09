use std::io::{self, IsTerminal};

use anyhow::{Context as _, Result, bail};
use clap::Args;
use dialoguer::Confirm;

use crate::actions::git::init::initialize;
use crate::ui::{self, style::DialogTheme};

use super::CLI;

#[derive(Args, Debug)]
pub struct Params {
    /// Clone from a Git URL or local repository path
    source: Option<String>,

    /// Set an existing checkout's canonical origin without prompting
    #[arg(long)]
    set_origin: bool,
}

pub async fn execute(cli: &CLI, params: &Params) -> Result<()> {
    tracing::debug!("init command");
    let destination = cli.resolve_home()?;
    let source = params.source.clone();
    let mut result =
        tokio::task::spawn_blocking(move || initialize(destination, source.as_deref()))
            .await
            .context("initialization task failed")??;
    if result.needs_origin_update() {
        confirm_origin_change(
            params.set_origin,
            io::stdin().is_terminal() && io::stderr().is_terminal(),
            || {
                let prompt = format!(
                    "Set origin from {} to {}?",
                    result.origin.as_deref().unwrap_or("<missing>"),
                    result.canonical.as_deref().unwrap_or("<missing>")
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
