//! Regenerate published schemas, or check them without modifying the working tree.

use std::path::Path;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let check = match args.as_slice() {
        [] => false,
        [arg] if arg == "--check" => true,
        _ => bail!("usage: cargo run --features schema --example generate_schemas -- [--check]"),
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schemas");
    generate(&directory, check)
}

fn generate(directory: &Path, check: bool) -> Result<()> {
    if !check {
        std::fs::create_dir_all(directory)?;
    }
    for (name, schema) in over::overlays::json_schema::schemas() {
        let path = directory.join(name);
        let content = format!("{}\n", serde_json::to_string_pretty(&schema)?);
        if check {
            let current = std::fs::read_to_string(&path).with_context(|| {
                format!("missing {}; run mise run schemas:generate", path.display())
            })?;
            if current != content {
                bail!("stale {}; run mise run schemas:generate", path.display());
            }
        } else {
            std::fs::write(&path, content)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn checking_missing_assets_fails_without_creating_them() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("missing");
        let error = generate(&directory, true).unwrap_err();
        assert!(error.to_string().contains("schemas:generate"));
        assert!(!directory.exists());
    }

    #[rstest]
    #[case("root.json")]
    #[case("overlay.json")]
    fn checking_a_missing_asset_preserves_the_other_asset(#[case] name: &str) {
        let temp = tempfile::tempdir().unwrap();
        generate(temp.path(), false).unwrap();
        let other_name = if name == "root.json" {
            "overlay.json"
        } else {
            "root.json"
        };
        let other_path = temp.path().join(other_name);
        let other_content = std::fs::read(&other_path).unwrap();
        let missing = temp.path().join(name);
        std::fs::remove_file(&missing).unwrap();
        assert!(
            generate(temp.path(), true)
                .unwrap_err()
                .to_string()
                .contains(name)
        );
        assert!(!missing.exists());
        assert_eq!(std::fs::read(other_path).unwrap(), other_content);
    }

    #[rstest]
    #[case("root.json")]
    #[case("overlay.json")]
    fn generation_is_stable_and_checking_stale_assets_never_rewrites_them(#[case] name: &str) {
        let temp = tempfile::tempdir().unwrap();
        generate(temp.path(), false).unwrap();
        generate(temp.path(), true).unwrap();
        let path = temp.path().join(name);
        let other_name = if name == "root.json" {
            "overlay.json"
        } else {
            "root.json"
        };
        let other_path = temp.path().join(other_name);
        let other_content = std::fs::read(&other_path).unwrap();
        let original = std::fs::read(&path).unwrap();
        generate(temp.path(), false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&path, "stale").unwrap();
        assert!(
            generate(temp.path(), true)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "stale");
        assert_eq!(std::fs::read(other_path).unwrap(), other_content);
    }
}
