//! Generate the JSON Schema assets published with the documentation site.

use std::path::Path;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    if std::env::args_os().len() > 1 {
        bail!("usage: cargo run --features schema --example generate_schemas");
    }
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schemas");
    generate(&directory)
}

fn generate(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    for (name, schema) in over::overlays::json_schema::schemas() {
        let path = directory.join(name);
        let content = format!("{}\n", serde_json::to_string_pretty(&schema)?);
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generating_both_schemas_produces_valid_deterministic_json() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("schemas");
        generate(&directory).unwrap();
        let originals: Vec<_> = ["root.json", "overlay.json"]
            .into_iter()
            .map(|name| {
                let content = std::fs::read_to_string(directory.join(name)).unwrap();
                let schema: serde_json::Value = serde_json::from_str(&content).unwrap();
                jsonschema::meta::validate(&schema).unwrap();
                assert_eq!(
                    schema["$id"],
                    format!("https://noirbizarre.github.io/over/schemas/{name}")
                );
                assert!(content.ends_with('\n'));
                (name, content)
            })
            .collect();
        generate(&directory).unwrap();
        for (name, original) in originals {
            assert_eq!(
                std::fs::read_to_string(directory.join(name)).unwrap(),
                original
            );
        }
    }

    #[test]
    fn a_write_failure_identifies_the_schema_file() {
        let temp = tempfile::tempdir().unwrap();
        // A directory at the output filename forces a portable write error, even as root.
        std::fs::create_dir(temp.path().join("root.json")).unwrap();
        assert!(
            generate(temp.path())
                .unwrap_err()
                .to_string()
                .contains("root.json")
        );
    }
}
