use std::sync::LazyLock;

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
