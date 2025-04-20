use std::path::PathBuf;

use crate::cli::CLI;
use crate::overlays::repository::Repository;
use anyhow::Result;

pub async fn execute(cli: &CLI) -> Result<()> {
    let home = cli.home.as_ref().expect("Home directory not set");
    let repo = Repository::new(PathBuf::from(home));
    println!("{:#?}", repo);
    Ok(())
}
