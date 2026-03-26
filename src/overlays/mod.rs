use std::fmt;
use std::sync::LazyLock;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Overlay files basename
pub(crate) const BASENAME: &str = "over";

/// Default overlay target directory
pub(crate) const DEFAULT_TARGET: &str = "~";

/// Overlay files extensions
pub(crate) const EXTENSIONS: &[&str] = &["yml", "yaml", "toml"];

pub mod overlay;
pub mod repository;

pub use overlay::Overlay;
pub use repository::Repository;

/// Overlay files search pattern
pub static GLOB_PATTERN: LazyLock<String> =
    LazyLock::new(|| format!("**/{}.{{{}}}", BASENAME, EXTENSIONS.join(",")));

/// Supported overlay descriptor formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Toml,
    Yaml,
}

impl Format {
    /// Returns the file extension for this format.
    pub fn extension(&self) -> &str {
        match self {
            Format::Toml => "toml",
            Format::Yaml => "yaml",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Format::Toml => write!(f, "toml"),
            Format::Yaml => write!(f, "yaml"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_default_is_toml() {
        assert_eq!(Format::default(), Format::Toml);
    }

    #[test]
    fn format_extension_toml() {
        assert_eq!(Format::Toml.extension(), "toml");
    }

    #[test]
    fn format_extension_yaml() {
        assert_eq!(Format::Yaml.extension(), "yaml");
    }

    #[test]
    fn format_display_toml() {
        assert_eq!(format!("{}", Format::Toml), "toml");
    }

    #[test]
    fn format_display_yaml() {
        assert_eq!(format!("{}", Format::Yaml), "yaml");
    }

    #[test]
    fn format_serde_roundtrip_toml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            format: Format,
        }
        let w = Wrapper {
            format: Format::Toml,
        };
        let serialized = toml::to_string(&w).unwrap();
        assert!(serialized.contains("format = \"toml\""));
        let deserialized: Wrapper = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.format, Format::Toml);
    }

    #[test]
    fn format_serde_roundtrip_yaml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            format: Format,
        }
        let w = Wrapper {
            format: Format::Yaml,
        };
        let serialized = toml::to_string(&w).unwrap();
        assert!(serialized.contains("format = \"yaml\""));
        let deserialized: Wrapper = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.format, Format::Yaml);
    }

    #[test]
    fn format_eq() {
        assert_eq!(Format::Toml, Format::Toml);
        assert_eq!(Format::Yaml, Format::Yaml);
        assert_ne!(Format::Toml, Format::Yaml);
    }

    #[test]
    #[allow(clippy::clone_on_copy)]
    fn format_clone_copy() {
        let f = Format::Yaml;
        let cloned = f.clone();
        let copied = f;
        assert_eq!(f, cloned);
        assert_eq!(f, copied);
    }

    #[test]
    fn format_debug() {
        assert_eq!(format!("{:?}", Format::Toml), "Toml");
        assert_eq!(format!("{:?}", Format::Yaml), "Yaml");
    }

    #[test]
    fn glob_pattern_contains_all_extensions() {
        let pattern = &*GLOB_PATTERN;
        for ext in EXTENSIONS {
            assert!(
                pattern.contains(ext),
                "GLOB_PATTERN missing extension {}",
                ext
            );
        }
        assert!(pattern.contains(BASENAME));
    }
}
