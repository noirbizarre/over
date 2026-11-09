use std::io::{self, Write};

use anyhow::{Context as _, Result, anyhow};
use clap::Args;

use crate::cli::CLI;
use crate::exec::Context;
use crate::overlays::Repository;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(help = "Name of the overlay to compose with the configured default context")]
    name: Option<String>,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    tracing::debug!(?cli, ?args, "CLI args");

    let repo = Repository::new(cli.resolve_home()?);
    let ctx = Context::builder()
        .repository(repo.clone())
        .no_prompt(true)
        .build();
    let selected = match &args.name {
        Some(name) => repo.get(name)?,
        None => repo.default_overlay(&ctx)?.ok_or_else(|| {
            anyhow!("no overlay selected; provide NAME or configure default_overlay")
        })?,
    };
    let data = repo.resolve_data(&ctx, Some(&selected))?;

    // Serialize before writing so a serialization error cannot leave partial JSON on stdout.
    let json = serde_json::to_string(&data).context("failed to serialize overlay data as JSON")?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{json}").context("failed to write overlay data to stdout")?;
    stdout.flush().context("failed to flush overlay data")?;
    Ok(())
}
