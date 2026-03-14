use crate::cli::CLI;
use crate::overlays::repository::Repository;
use crate::ui::style;
use crate::utils::short_path;
use anyhow::Result;

pub async fn execute(cli: &CLI) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home);

    println!(
        "{} {}",
        style::white_b("Repository:"),
        short_path(&repo.root.to_string_lossy()),
    );

    let overlays = repo.overlays()?;
    if overlays.is_empty() {
        println!("  No overlays found.");
    } else {
        println!("  {} overlay(s):", overlays.len(),);
        for overlay in &overlays {
            let desc = overlay.description.as_deref().unwrap_or("");
            if desc.is_empty() {
                println!("    {} -> {}", style::cyan(&overlay.name), &overlay.target);
            } else {
                println!(
                    "    {} -> {} ({})",
                    style::cyan(&overlay.name),
                    &overlay.target,
                    desc,
                );
            }
        }
    }
    Ok(())
}
