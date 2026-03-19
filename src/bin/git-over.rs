use anyhow::Result;
use dot_over::cli::git_over;

#[tokio::main]
async fn main() -> Result<()> {
    git_over::main().await?;
    Ok(())
}
