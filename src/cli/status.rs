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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    #[tokio::test]
    async fn status_empty_repository() {
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_single_overlay_no_description() {
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_overlay_with_description() {
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("described");
        fs::create_dir_all(&ov).unwrap();
        fs::write(
            ov.join("over.toml"),
            "target = \"~\"\ndescription = \"My described overlay\"",
        )
        .unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn status_multiple_overlays() {
        let tmp = TempDir::new().unwrap();
        for (name, has_desc) in [("first", true), ("second", false), ("third", true)] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            let content = if has_desc {
                format!("target = \"~\"\ndescription = \"{}\"", name)
            } else {
                "target = \"~\"".to_string()
            };
            fs::write(ov.join("over.toml"), content).unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let result = execute(&cli).await;
        assert!(result.is_ok());
    }
}
