use std::io::{IsTerminal, Read, Write};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Subcommand;

use crate::secrets::{CryptoService, SecretBytes, SecretName, SecretStore};

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Read and encrypt a named secret
    Add {
        name: String,
        #[arg(long)]
        replace: bool,
    },
    /// List secret names using the repository unlock state
    List,
    /// Write a decrypted secret to stdout
    Get { name: String },
    /// Remove an encrypted secret
    Remove { name: String },
}

pub async fn execute(cli: &super::CLI, command: &Commands) -> Result<()> {
    let store = SecretStore::new(&cli.resolve_home()?, Arc::new(CryptoService::default()))?;
    let command = command.clone();
    // Crypto and terminal input are blocking; keep them off the async executor.
    tokio::task::spawn_blocking(move || -> Result<()> {
        match command {
            Commands::Add { name, replace } => {
                let name = SecretName::parse(&name)?;
                store.check_add(&name, replace)?;
                let mut input = zeroize::Zeroizing::new(Vec::new());
                if !std::io::stdin().is_terminal() {
                    std::io::stdin()
                        .read_to_end(&mut input)
                        .context("could not read secret from stdin")?;
                }
                if input.is_empty() {
                    let value = zeroize::Zeroizing::new(crate::secrets::crypto::hidden_input(
                        "Secret value",
                    )?);
                    input.extend_from_slice(value.as_bytes());
                }
                store.add(
                    &name,
                    &SecretBytes::new(std::mem::take(&mut *input)),
                    replace,
                    true,
                )?;
            }
            Commands::List => {
                for name in store.list()? {
                    println!("{name}");
                }
            }
            Commands::Get { name } => {
                let value = store.get(&SecretName::parse(&name)?, true)?;
                let mut stdout = std::io::stdout().lock();
                stdout
                    .write_all(value.expose())
                    .context("could not write secret to stdout")?;
                stdout.flush().context("could not flush stdout")?;
            }
            Commands::Remove { name } => store.remove(&SecretName::parse(&name)?)?,
        }
        Ok(())
    })
    .await
    .context("secret operation failed")?
}
