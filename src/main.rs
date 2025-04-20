use anyhow::Result;
use dot_over::cli;

// fn main() -> Result<(), Box<dyn std::error::Error>> {
//     cli::main()?;
//     Ok(())
// }

#[tokio::main]
async fn main() -> Result<()> {
    cli::main().await?;
    Ok(())
}
