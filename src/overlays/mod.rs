use std::fmt;
use std::sync::LazyLock;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Overlay files basename
pub(crate) const BASENAME: &str = "over";

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
